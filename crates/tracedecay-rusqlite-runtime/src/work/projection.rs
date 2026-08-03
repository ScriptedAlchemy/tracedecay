//! Bounded, generation-bound projection snapshot and delta reads.

use super::events::load_registered_projection;
use super::*;

impl WorkProjectionReadPort for WorkSqliteStorage {
    fn exact_snapshot(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        exact_snapshot_registered(&self.handle, authority, task_id)
    }

    fn snapshot(
        &self,
        authority: &WorkAuthority,
        page_size: u32,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        snapshot_registered(&self.handle, authority, page_size)
    }

    fn delta(
        &self,
        authority: &WorkAuthority,
        cursor: &WorkProjectionResumeCursorV1,
        page_size: u32,
    ) -> Result<WorkProjectionDeltaV1, WorkProjectionPortError> {
        delta_registered(&self.handle, authority, cursor, page_size)
    }
}

pub(crate) fn exact_snapshot_registered(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
    task_id: &TaskId,
) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
    let generation_id = projection_generation(authority)?;
    let sequence = WorkProjectionSequenceV1::new(registered_owner_cursor(handle, authority)?);
    let projection = load_registered_projection(handle, authority, task_id)
        .map_err(|_| WorkProjectionPortError::Unavailable)?;
    WorkProjectionSnapshotV1::new(
        generation_id,
        sequence,
        vec![projection],
        WorkProjectionCoverageV1::complete(1, 1)
            .map_err(|_| WorkProjectionPortError::Unavailable)?,
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

pub(crate) fn snapshot_registered(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
    page_size: u32,
) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
    let generation_id = projection_generation(authority)?;
    let sequence = WorkProjectionSequenceV1::new(registered_owner_cursor(handle, authority)?);
    let total = registered_count(
        &registered_work_query(
            handle,
            "SELECT COUNT(*) FROM work_projection_snapshots_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5",
            authority_params_owned(authority),
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?,
    )?;
    let rows = registered_work_query(
        handle,
        "SELECT projection_payload FROM work_projection_snapshots_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
         ORDER BY task_id LIMIT ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Integer(i64::from(page_size))])
            .collect(),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)?;
    let projections = decode_registered_projections(rows)?;
    let returned =
        u32::try_from(projections.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
    let coverage = if returned == total {
        WorkProjectionCoverageV1::complete(returned, total)
            .map_err(|_| WorkProjectionPortError::Unavailable)?
    } else {
        let range = WorkProjectionSequenceRangeV1::new(WorkProjectionSequenceV1::new(0), sequence)
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        WorkProjectionCoverageV1::capped(
            returned,
            total,
            page_size,
            range,
            projection_cursor(generation_id.clone(), sequence)?,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?
    };
    WorkProjectionSnapshotV1::new(generation_id, sequence, projections, coverage)
        .map_err(|_| WorkProjectionPortError::Unavailable)
}

pub(crate) fn delta_registered(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
    cursor: &WorkProjectionResumeCursorV1,
    page_size: u32,
) -> Result<WorkProjectionDeltaV1, WorkProjectionPortError> {
    let generation_id = projection_generation(authority)?;
    if cursor.generation_id() != &generation_id {
        return Err(WorkProjectionPortError::StaleCursor);
    }
    let from = parse_projection_cursor(cursor)?;
    let from_sql = i64::try_from(from).map_err(|_| WorkProjectionPortError::StaleCursor)?;
    let current = registered_owner_cursor(handle, authority)?;
    if from >= current {
        return Err(WorkProjectionPortError::StaleCursor);
    }
    let total = registered_count(
        &registered_work_query(
            handle,
            "SELECT COUNT(DISTINCT task_id) FROM work_projection_deltas_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5 AND owner_sequence > ?6",
            authority_params_owned(authority)
                .into_iter()
                .chain([ExactSqlValue::Integer(from_sql)])
                .collect(),
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?,
    )?;
    let rows = registered_work_query(
        handle,
        "WITH latest AS (
            SELECT task_id, MAX(owner_sequence) AS owner_sequence
            FROM work_projection_deltas_v1
            WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
              AND actor_id = ?4 AND policy_digest = ?5 AND owner_sequence > ?6
            GROUP BY task_id
         )
         SELECT delta.projection_payload, latest.owner_sequence
         FROM latest JOIN work_projection_deltas_v1 AS delta
           ON delta.project_id = ?1 AND delta.repository_id = ?2 AND delta.worktree_id = ?3
          AND delta.actor_id = ?4 AND delta.policy_digest = ?5
          AND delta.task_id = latest.task_id
          AND delta.owner_sequence = latest.owner_sequence
         ORDER BY latest.owner_sequence LIMIT ?7",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                ExactSqlValue::Integer(from_sql),
                ExactSqlValue::Integer(i64::from(page_size)),
            ])
            .collect(),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)?;
    let mut changed = Vec::new();
    let mut to = from;
    for row in rows.rows {
        let payload = exact_sql_text(&row.values, 0).ok_or(WorkProjectionPortError::Unavailable)?;
        changed
            .push(serde_json::from_str(payload).map_err(|_| WorkProjectionPortError::Unavailable)?);
        to = to.max(
            u64::try_from(
                exact_sql_integer(&row.values, 1).ok_or(WorkProjectionPortError::Unavailable)?,
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?,
        );
    }
    let returned =
        u32::try_from(changed.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
    if returned == total {
        to = current;
    }
    let from = WorkProjectionSequenceV1::new(from);
    let to = WorkProjectionSequenceV1::new(to);
    let coverage = if returned == total {
        WorkProjectionCoverageV1::complete(returned, total)
            .map_err(|_| WorkProjectionPortError::Unavailable)?
    } else {
        let range = WorkProjectionSequenceRangeV1::new(from, to)
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        WorkProjectionCoverageV1::capped(
            returned,
            total,
            page_size,
            range,
            projection_cursor(generation_id.clone(), to)?,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?
    };
    WorkProjectionDeltaV1::new(generation_id, from, to, changed, BTreeSet::new(), coverage)
        .map_err(|_| WorkProjectionPortError::Unavailable)
}

pub(crate) fn registered_owner_cursor(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
) -> Result<u64, WorkProjectionPortError> {
    let rows = registered_work_query(
        handle,
        "SELECT sequence FROM work_owner_cursors_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5",
        authority_params_owned(authority),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)?;
    match rows.rows.first() {
        None => Ok(0),
        Some(row) => u64::try_from(
            exact_sql_integer(&row.values, 0).ok_or(WorkProjectionPortError::Unavailable)?,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable),
    }
}

pub(crate) fn registered_count(rows: &ExactSqlRows) -> Result<u32, WorkProjectionPortError> {
    u32::try_from(
        rows.rows
            .first()
            .and_then(|row| exact_sql_integer(&row.values, 0))
            .ok_or(WorkProjectionPortError::Unavailable)?,
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

pub(crate) fn decode_registered_projections(
    rows: ExactSqlRows,
) -> Result<Vec<WorkProjection>, WorkProjectionPortError> {
    rows.rows
        .into_iter()
        .map(|row| {
            serde_json::from_str(
                exact_sql_text(&row.values, 0).ok_or(WorkProjectionPortError::Unavailable)?,
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)
        })
        .collect()
}

pub(crate) fn projection_generation(
    authority: &WorkAuthority,
) -> Result<ProjectionGenerationId, WorkProjectionPortError> {
    let digest = canonical_sha256(&("tracedecay.work.projection.generation.v1", authority))
        .map_err(|_| WorkProjectionPortError::Unavailable)?;
    ProjectionGenerationId::try_from(format!(
        "generation.work.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

pub(crate) fn projection_cursor(
    generation_id: ProjectionGenerationId,
    sequence: WorkProjectionSequenceV1,
) -> Result<WorkProjectionResumeCursorV1, WorkProjectionPortError> {
    WorkProjectionResumeCursorV1::new(
        generation_id,
        format!("work-projection-sequence.v1:{}", sequence.get()),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

pub(crate) fn parse_projection_cursor(
    cursor: &WorkProjectionResumeCursorV1,
) -> Result<u64, WorkProjectionPortError> {
    cursor
        .token()
        .strip_prefix("work-projection-sequence.v1:")
        .and_then(|sequence| sequence.parse::<u64>().ok())
        .ok_or(WorkProjectionPortError::StaleCursor)
}

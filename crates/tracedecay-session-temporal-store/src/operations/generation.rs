use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::json;
use tracedecay_runtime_core::db::engine::params;

use tracedecay_lcm::types::{LcmError, LcmImmutableSummaryPublication};

use super::PUBLICATION_ROUTE;
use crate::relations::{SessionRelationProjection, SummarySourceRef};
use crate::sql::GENERATION_COPY_STATEMENTS;

const MAX_LINEAGE_DEPTH: usize = 64;
const MAX_LINEAGE_NODES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawSummaryInvalidation {
    pub rewind_frontier_store_id: i64,
    pub stale_summary_count: usize,
    pub work_count: usize,
    pub has_more: bool,
}

/// Starts a new canonical generation in which every summary transitively
/// derived from a revised raw source is stale. The immutable historical nodes
/// remain auditable, but no active-generation retrieval can return them.
pub async fn invalidate_raw_summary_revision(
    conn: &impl crate::handle::SessionTemporalExec,
    provider: &str,
    session_id: &str,
    raw_store_id: i64,
    max_affected: usize,
) -> Result<RawSummaryInvalidation, LcmError> {
    if max_affected == 0 {
        return Err(lineage_limit(
            session_id,
            "raw_revision_invalidation_budget_exhausted",
        ));
    }
    let Some(active) = active_generation(conn, session_id).await? else {
        return Ok(RawSummaryInvalidation {
            rewind_frontier_store_id: raw_store_id.saturating_sub(1).max(0),
            stale_summary_count: 0,
            work_count: 0,
            has_more: false,
        });
    };
    conn.execute(
        "INSERT INTO lcm_summary_convergence_invalidation_work (
             provider, session_id, raw_store_id,
             source_kind, source_id, depth, after_node_id
         ) VALUES (?1, ?2, ?3, 'raw_message', CAST(?3 AS TEXT), 0, '')
         ON CONFLICT(provider, session_id, raw_store_id, source_kind, source_id)
         DO NOTHING",
        params![provider, session_id, raw_store_id],
    )
    .await?;
    let now = super::unixepoch(conn).await?;
    let mut work_count = 0_usize;
    let mut stale_summary_count = 0_usize;
    while work_count < max_affected {
        let mut work_rows = conn
            .query(
                "SELECT source_kind, source_id, depth, after_node_id
                 FROM lcm_summary_convergence_invalidation_work
                 WHERE provider = ?1 AND session_id = ?2 AND raw_store_id = ?3
                   AND state = 'pending'
                 ORDER BY depth, source_kind, source_id
                 LIMIT 1",
                params![provider, session_id, raw_store_id],
            )
            .await?;
        let Some(work_row) = work_rows.next().await? else {
            break;
        };
        let source_kind = work_row.get::<String>(0)?;
        let source_id = work_row.get::<String>(1)?;
        let depth = work_row.get::<i64>(2)?;
        let after_node_id = work_row.get::<String>(3)?;
        drop(work_rows);
        let remaining = max_affected.saturating_sub(work_count);
        let query_limit = i64::try_from(remaining.saturating_add(1))
            .map_err(|_| lineage_limit(session_id, "raw_revision_invalidation_budget_exhausted"))?;
        let mut rows = conn
            .query(
                "SELECT node_id
                 FROM lcm_summary_sources
                 WHERE source_kind = ?1 AND source_id = ?2 AND node_id > ?3
                 ORDER BY node_id
                 LIMIT ?4",
                params![
                    source_kind.as_str(),
                    source_id.as_str(),
                    after_node_id,
                    query_limit
                ],
            )
            .await?;
        let mut discovered = Vec::with_capacity(remaining.saturating_add(1));
        while let Some(row) = rows.next().await? {
            discovered.push(row.get::<String>(0)?);
        }
        drop(rows);
        let source_drained = discovered.len() <= remaining;
        discovered.truncate(remaining);
        if discovered.is_empty() {
            conn.execute(
                "UPDATE lcm_summary_convergence_invalidation_work
                 SET state = 'drained'
                 WHERE provider = ?1 AND session_id = ?2 AND raw_store_id = ?3
                   AND source_kind = ?4 AND source_id = ?5",
                params![provider, session_id, raw_store_id, source_kind, source_id],
            )
            .await?;
            work_count = work_count.saturating_add(1);
            continue;
        }
        for summary_id in &discovered {
            conn.execute(
                "INSERT INTO lcm_summary_convergence_invalidation_work (
                     provider, session_id, raw_store_id,
                     source_kind, source_id, depth, after_node_id
                 ) VALUES (?1, ?2, ?3, 'summary_node', ?4, ?5, '')
                 ON CONFLICT(provider, session_id, raw_store_id, source_kind, source_id)
                 DO NOTHING",
                params![
                    provider,
                    session_id,
                    raw_store_id,
                    summary_id,
                    depth.saturating_add(1)
                ],
            )
            .await?;
            let changed = conn
                .execute(
                    "UPDATE session_summary_availability
                     SET availability = 'stale', reason = 'raw_source_revised', checked_at = ?4
                     WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3
                       AND availability = 'available'",
                    params![session_id, active, summary_id, now],
                )
                .await?;
            stale_summary_count =
                stale_summary_count.saturating_add(usize::try_from(changed).map_err(|_| {
                    lineage_limit(session_id, "raw_revision_invalidation_count_overflow")
                })?);
            let mut source_rows = conn
                .query(
                    "SELECT MIN(CAST(source_id AS INTEGER))
                     FROM lcm_summary_sources
                     WHERE node_id = ?1 AND source_kind = 'raw_message'",
                    params![summary_id],
                )
                .await?;
            let first_raw_source = source_rows
                .next()
                .await?
                .ok_or_else(|| LcmError::Db("summary source query returned no row".into()))?
                .get::<Option<i64>>(0)?;
            drop(source_rows);
            if let Some(first_raw_source) = first_raw_source {
                conn.execute(
                    "UPDATE lcm_summary_convergence_dirty_raw
                     SET rewind_frontier_store_id = MIN(
                         rewind_frontier_store_id, MAX(0, ?4 - 1)
                     )
                     WHERE provider = ?1 AND session_id = ?2 AND store_id = ?3",
                    params![provider, session_id, raw_store_id, first_raw_source],
                )
                .await?;
            }
        }
        work_count = work_count.saturating_add(discovered.len());
        if source_drained {
            conn.execute(
                "UPDATE lcm_summary_convergence_invalidation_work
                 SET state = 'drained'
                 WHERE provider = ?1 AND session_id = ?2 AND raw_store_id = ?3
                   AND source_kind = ?4 AND source_id = ?5",
                params![provider, session_id, raw_store_id, source_kind, source_id],
            )
            .await?;
        } else if let Some(last_node_id) = discovered.last() {
            conn.execute(
                "UPDATE lcm_summary_convergence_invalidation_work
                 SET after_node_id = ?6
                 WHERE provider = ?1 AND session_id = ?2 AND raw_store_id = ?3
                   AND source_kind = ?4 AND source_id = ?5",
                params![
                    provider,
                    session_id,
                    raw_store_id,
                    source_kind,
                    source_id,
                    last_node_id,
                ],
            )
            .await?;
        }
    }
    let mut remaining_rows = conn
        .query(
            "SELECT 1 FROM lcm_summary_convergence_invalidation_work
             WHERE provider = ?1 AND session_id = ?2 AND raw_store_id = ?3
               AND state = 'pending'
             LIMIT 1",
            params![provider, session_id, raw_store_id],
        )
        .await?;
    let has_more = remaining_rows.next().await?.is_some();
    let mut rewind_rows = conn
        .query(
            "SELECT rewind_frontier_store_id
             FROM lcm_summary_convergence_dirty_raw
             WHERE provider = ?1 AND session_id = ?2 AND store_id = ?3",
            params![provider, session_id, raw_store_id],
        )
        .await?;
    let rewind_frontier_store_id = rewind_rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("dirty raw revision disappeared during invalidation".into()))?
        .get::<i64>(0)?;
    Ok(RawSummaryInvalidation {
        rewind_frontier_store_id,
        stale_summary_count,
        work_count,
        has_more,
    })
}

pub(super) fn validate_lineage_projection(
    projection: &SessionRelationProjection,
    publication: &LcmImmutableSummaryPublication,
) -> Result<(), LcmError> {
    let summary_id = publication.summary_id.as_str();
    let sources = projection
        .summaries
        .iter()
        .map(|summary| {
            (
                summary.summary_id.as_str(),
                summary
                    .sources
                    .iter()
                    .filter_map(|source| match source {
                        SummarySourceRef::Summary { summary_id } => Some(summary_id.as_str()),
                        SummarySourceRef::Anchor { .. } => None,
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for source in &publication.draft.source_refs {
        let tracedecay_lcm::types::LcmSourceRef::SummaryNode { node_id } = source else {
            continue;
        };
        if node_id == summary_id {
            return Err(cycle(summary_id));
        }
        let mut queue = VecDeque::from([(node_id.as_str(), 0_usize)]);
        let mut expanded = BTreeSet::new();
        while let Some((node, depth)) = queue.pop_front() {
            if depth > MAX_LINEAGE_DEPTH {
                return Err(lineage_limit(summary_id, "lineage_depth_exceeded"));
            }
            if !expanded.insert(node) {
                continue;
            }
            if expanded.len() > MAX_LINEAGE_NODES {
                return Err(lineage_limit(summary_id, "lineage_node_limit_exceeded"));
            }
            if node == summary_id {
                return Err(cycle(summary_id));
            }
            for next in sources.get(node).into_iter().flatten() {
                queue.push_back((next, depth + 1));
            }
        }
    }
    if publication.predecessor_summary_id.as_deref() == Some(summary_id) {
        return Err(cycle(summary_id));
    }
    Ok(())
}

#[hotpath::measure(
    future = true,
    label = "session_temporal.publication.validate_predecessor"
)]
pub(super) async fn validate_current_predecessor(
    conn: &impl crate::handle::SessionTemporalExec,
    projection: &SessionRelationProjection,
    publication: &LcmImmutableSummaryPublication,
    logical_identity_digest: &str,
) -> Result<(), LcmError> {
    let summary_id = publication.summary_id.as_str();
    let superseded = projection
        .summaries
        .iter()
        .filter_map(|summary| summary.predecessor_summary_id.as_deref())
        .collect::<BTreeSet<_>>();
    let mut matching = conn
        .query(
            "SELECT node.summary_id, node.publication_json
             FROM session_summary_nodes AS node
             JOIN session_temporal_generations AS generation
               ON generation.session_id = node.session_id
              AND generation.state = 'active'
             JOIN session_summary_availability AS availability
               ON availability.session_id = node.session_id
              AND availability.generation = generation.generation
              AND availability.summary_id = node.summary_id
              AND availability.availability = 'available'
             WHERE node.session_id = ?1
             ORDER BY node.created_at, node.summary_id",
            params![publication.draft.session_id.as_str()],
        )
        .await?;
    let mut current_for_identity = Vec::new();
    while let Some(row) = matching.next().await? {
        hotpath::gauge!("session_temporal.publication.predecessor_manifest_rows").inc(1_u64);
        let candidate_id: String = row.get(0)?;
        let manifest_raw: String = row.get(1)?;
        let manifest = serde_json::from_str::<super::CanonicalPublicationManifest>(&manifest_raw)
            .map_err(|_| LcmError::ImmutableSummaryConflict {
            summary_id: candidate_id.clone(),
        })?;
        if manifest.logical_identity_digest != logical_identity_digest {
            continue;
        }
        if !superseded.contains(candidate_id.as_str()) {
            current_for_identity.push(candidate_id);
        }
    }

    match publication.predecessor_summary_id.as_deref() {
        None if current_for_identity.is_empty() => Ok(()),
        None if current_for_identity.len() == 1 => Err(LcmError::SummaryPredecessorRequired {
            summary_id: summary_id.to_string(),
            current_predecessor_id: current_for_identity.remove(0),
        }),
        None => Err(LcmError::ImmutableSummaryConflict {
            summary_id: summary_id.to_string(),
        }),
        Some(predecessor) => {
            let Some((manifest, _)) = super::load_manifest(conn, predecessor).await? else {
                return Err(LcmError::SummaryNodeNotFound);
            };
            if manifest.session_id != publication.draft.session_id
                || manifest.provider != publication.draft.provider
                || manifest.logical_identity_digest != logical_identity_digest
                || current_for_identity.len() != 1
                || current_for_identity.first().map(String::as_str) != Some(predecessor)
            {
                return Err(LcmError::InvalidSummarySuccessor {
                    summary_id: summary_id.to_string(),
                    predecessor_summary_id: predecessor.to_string(),
                });
            }
            Ok(())
        }
    }
}

#[hotpath::measure(future = true, label = "session_temporal.persist.publish_generation")]
pub(super) async fn publish_candidate_generation(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &str,
    summary_id: &str,
    predecessor: Option<&str>,
    source_horizon_json: &str,
    now: i64,
    relation_projection: &SessionRelationProjection,
) -> Result<i64, LcmError> {
    let active = active_generation(conn, session_id).await?;
    let mut max_rows = conn
        .query(
            "SELECT COALESCE(MAX(generation), 0)
             FROM session_temporal_generations WHERE session_id = ?1",
            params![session_id],
        )
        .await?;
    let max_generation: i64 = max_rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("max generation query returned no row".to_string()))?
        .get(0)?;
    let candidate = max_generation + 1;
    let (source_frontier, projection_frontier, summary_frontier, cursor_key) =
        if let Some(active) = active {
            let mut rows = conn
                .query(
                    "SELECT frozen_watermarks_json
                     FROM session_temporal_generations
                     WHERE session_id = ?1 AND generation = ?2 AND state = 'active'",
                    params![session_id, active],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Err(stale_generation(conn, session_id, active).await?);
            };
            let encoded = row.get::<String>(0)?;
            let frozen: serde_json::Value = serde_json::from_str(&encoded)
                .map_err(|error| LcmError::Db(format!("invalid active watermarks: {error}")))?;
            (
                frozen["source_frontier"].as_u64().unwrap_or_default(),
                frozen["projection_frontier"].as_u64().unwrap_or_default(),
                frozen["summary_frontier"].as_u64().unwrap_or_default(),
                frozen["cursor_key"].clone(),
            )
        } else {
            (0, 0, 0, serde_json::Value::Null)
        };
    let watermarks = json!({
        "active_generation": active.unwrap_or(candidate),
        "cursor_key": cursor_key,
        "source_frontier": source_frontier,
        "projection_frontier": projection_frontier,
        "summary_frontier": summary_frontier.saturating_add(1),
        "route": PUBLICATION_ROUTE,
    })
    .to_string();
    conn.execute(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES (?1, ?2, 'building', ?3, ?4)",
        params![session_id, candidate, watermarks.as_str(), now],
    )
    .await?;
    if let Some(active) = active {
        copy_active_projection(conn, session_id, active, candidate).await?;
        conn.execute(
            "INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             )
             SELECT session_id, ?2, summary_id, availability,
                    source_horizon_json, reason, ?3
             FROM session_summary_availability
             WHERE session_id = ?1 AND generation = ?4",
            params![session_id, candidate, now, active],
        )
        .await?;
    }
    if let Some(predecessor) = predecessor {
        for affected in stale_closure(relation_projection, predecessor, summary_id)? {
            let mut rows = conn
                .query(
                    "SELECT source_horizon_json
                     FROM session_summary_nodes WHERE summary_id = ?1",
                    params![affected.as_str()],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Err(LcmError::SummaryNodeNotFound);
            };
            let horizon: String = row.get(0)?;
            conn.execute(
                "INSERT INTO session_summary_availability (
                    session_id, generation, summary_id, availability,
                    source_horizon_json, reason, checked_at
                 ) VALUES (?1, ?2, ?3, 'stale', ?4, 'predecessor_superseded', ?5)
                 ON CONFLICT(session_id, generation, summary_id) DO UPDATE SET
                    availability = 'stale',
                    source_horizon_json = excluded.source_horizon_json,
                    reason = 'predecessor_superseded',
                    checked_at = excluded.checked_at",
                params![session_id, candidate, affected.as_str(), horizon, now],
            )
            .await?;
        }
    }
    conn.execute(
        "INSERT INTO session_summary_availability (
            session_id, generation, summary_id, availability,
            source_horizon_json, reason, checked_at
         ) VALUES (?1, ?2, ?3, 'available', ?4, NULL, ?5)",
        params![session_id, candidate, summary_id, source_horizon_json, now],
    )
    .await?;
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = ?3
         WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
        params![session_id, candidate, now],
    )
    .await?;
    if let Some(expected) = active {
        let changed = conn
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'superseded', completed_at = MAX(?3, activated_at)
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'active'",
                params![session_id, expected, now],
            )
            .await?;
        if changed != 1 {
            return Err(stale_generation(conn, session_id, expected).await?);
        }
    }
    let activated = conn
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = ?3
             WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'
               AND NOT EXISTS (
                   SELECT 1 FROM session_temporal_generations
                   WHERE session_id = ?1 AND state = 'active'
               )",
            params![session_id, candidate, now],
        )
        .await?;
    if activated != 1 {
        return Err(stale_generation(conn, session_id, candidate).await?);
    }
    Ok(candidate)
}

async fn stale_generation(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &str,
    expected: i64,
) -> Result<LcmError, LcmError> {
    Ok(LcmError::StaleSummaryGeneration {
        expected,
        actual: active_generation(conn, session_id)
            .await?
            .unwrap_or_default(),
    })
}

fn stale_closure(
    projection: &SessionRelationProjection,
    predecessor: &str,
    conflict_id: &str,
) -> Result<Vec<String>, LcmError> {
    let dependents = projection
        .summaries
        .iter()
        .flat_map(|summary| {
            summary
                .sources
                .iter()
                .filter_map(move |source| match source {
                    SummarySourceRef::Summary { summary_id } => {
                        Some((summary_id.as_str(), summary.summary_id.as_str()))
                    }
                    SummarySourceRef::Anchor { .. } => None,
                })
        })
        .fold(
            BTreeMap::<_, Vec<_>>::new(),
            |mut graph, (source, dependent)| {
                graph.entry(source).or_default().push(dependent);
                graph
            },
        );
    let mut queue = VecDeque::from([(predecessor, 0usize)]);
    let mut expanded = BTreeSet::new();
    let mut affected = Vec::new();
    while let Some((node, depth)) = queue.pop_front() {
        if depth > MAX_LINEAGE_DEPTH {
            return Err(lineage_limit(conflict_id, "lineage_depth_exceeded"));
        }
        if !expanded.insert(node) {
            continue;
        }
        if expanded.len() > MAX_LINEAGE_NODES {
            return Err(lineage_limit(conflict_id, "lineage_node_limit_exceeded"));
        }
        affected.push(node.to_owned());
        for next in dependents.get(node).into_iter().flatten() {
            queue.push_back((next, depth + 1));
        }
    }
    Ok(affected)
}

fn lineage_limit(summary_id: &str, reason: &str) -> LcmError {
    LcmError::SummarySourceUnavailable {
        source_id: summary_id.to_string(),
        reason: reason.to_string(),
    }
}

fn cycle(summary_id: &str) -> LcmError {
    LcmError::SummaryCycle {
        summary_id: summary_id.to_string(),
    }
}

async fn copy_active_projection(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &str,
    active: i64,
    candidate: i64,
) -> Result<(), LcmError> {
    for sql in GENERATION_COPY_STATEMENTS {
        conn.execute(sql, params![session_id, candidate, active])
            .await?;
    }
    Ok(())
}

pub(super) async fn active_generation(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &str,
) -> Result<Option<i64>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT generation FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active'
             ORDER BY generation",
            params![session_id],
        )
        .await?;
    let active = rows.next().await?.map(|row| row.get(0)).transpose()?;
    if rows.next().await?.is_some() {
        return Err(LcmError::StaleSummaryGeneration {
            expected: active.unwrap_or_default(),
            actual: active.unwrap_or_default(),
        });
    }
    Ok(active)
}

pub(super) async fn generation_watermarks(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &str,
    generation: i64,
) -> Result<String, LcmError> {
    let mut rows = conn
        .query(
            "SELECT frozen_watermarks_json
             FROM session_temporal_generations
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id, generation],
        )
        .await?;
    rows.next()
        .await?
        .ok_or(LcmError::StaleSummaryGeneration {
            expected: generation,
            actual: 0,
        })?
        .get(0)
        .map_err(LcmError::from)
}

#[cfg(test)]
mod tests {
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection, params};

    use super::invalidate_raw_summary_revision;

    #[tokio::test]
    async fn raw_revision_invalidation_pages_beyond_the_former_lineage_cap() {
        const SUMMARY_COUNT: i64 = 4_098;
        const PAGE_LIMIT: usize = 512;
        let temp = tempfile::tempdir().unwrap();
        let conn = TestConnection::open(&temp.path().join("sessions.db"));
        conn.execute_batch(
            "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL
             );
             CREATE TABLE lcm_summary_sources (
                node_id TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_id TEXT NOT NULL
             );
             CREATE TABLE session_summary_availability (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                summary_id TEXT NOT NULL,
                availability TEXT NOT NULL,
                reason TEXT,
                checked_at INTEGER NOT NULL
             );
             CREATE INDEX idx_test_availability
                ON session_summary_availability(session_id, generation, availability);
             CREATE TABLE lcm_summary_convergence_dirty_raw (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                store_id INTEGER NOT NULL,
                rewind_frontier_store_id INTEGER NOT NULL,
                PRIMARY KEY(provider, session_id, store_id)
             );
             CREATE TABLE lcm_summary_convergence_invalidation_work (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                raw_store_id INTEGER NOT NULL,
                source_kind TEXT NOT NULL,
                source_id TEXT NOT NULL,
                depth INTEGER NOT NULL,
                after_node_id TEXT NOT NULL DEFAULT '',
                state TEXT NOT NULL DEFAULT 'pending',
                PRIMARY KEY(provider, session_id, raw_store_id, source_kind, source_id)
             );
             INSERT INTO session_temporal_generations(session_id, generation, state)
             VALUES ('large-closure', 1, 'active');
             INSERT INTO lcm_summary_convergence_dirty_raw(
                 provider, session_id, store_id, rewind_frontier_store_id
             )
             VALUES ('cursor', 'large-closure', 100, 99);
             WITH RECURSIVE ids(value) AS (
                VALUES(1) UNION ALL SELECT value + 1 FROM ids WHERE value < 4098
             )
             INSERT INTO lcm_summary_sources(node_id, source_kind, source_id)
             SELECT printf('summary-%05d', value), 'raw_message', '100' FROM ids;
             INSERT INTO lcm_summary_sources(node_id, source_kind, source_id)
             VALUES ('summary-00001', 'raw_message', '5');
             WITH RECURSIVE ids(value) AS (
                VALUES(1) UNION ALL SELECT value + 1 FROM ids WHERE value < 4098
             )
             INSERT INTO session_summary_availability(
                session_id, generation, summary_id, availability, reason, checked_at
             )
             SELECT 'large-closure', 1, printf('summary-%05d', value),
                    'available', NULL, 0
             FROM ids;",
        )
        .await
        .unwrap();

        let mut affected = 0_usize;
        let mut final_rewind = None;
        for _ in 0..32 {
            let page =
                invalidate_raw_summary_revision(&conn, "cursor", "large-closure", 100, PAGE_LIMIT)
                    .await
                    .unwrap();
            assert!(page.stale_summary_count <= PAGE_LIMIT);
            affected = affected.saturating_add(page.stale_summary_count);
            final_rewind = Some(page.rewind_frontier_store_id);
            if !page.has_more {
                break;
            }
        }
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM session_summary_availability
                 WHERE session_id = 'large-closure' AND generation = 1
                   AND availability = 'available'",
                params![],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0,
            "bounded pages must not strand descendants beyond an internal traversal cap"
        );
        assert_eq!(affected, SUMMARY_COUNT as usize);
        assert_eq!(final_rewind, Some(4));
    }

    #[tokio::test]
    async fn raw_revision_invalidation_visits_diamond_nodes_once_across_pages() {
        const PAGE_LIMIT: usize = 1;
        let temp = tempfile::tempdir().unwrap();
        let conn = TestConnection::open(&temp.path().join("sessions.db"));
        conn.execute_batch(
            "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL
             );
             CREATE TABLE lcm_summary_sources (
                node_id TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_id TEXT NOT NULL
             );
             CREATE TABLE lcm_summary_nodes (
                node_id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                depth INTEGER NOT NULL,
                summary_text TEXT NOT NULL,
                summary_hash TEXT NOT NULL,
                summary_token_count INTEGER NOT NULL,
                source_token_count INTEGER NOT NULL,
                source_time_start INTEGER,
                source_time_end INTEGER,
                expand_hint TEXT,
                metadata_json TEXT,
                created_at INTEGER NOT NULL
             );
             CREATE TABLE session_summary_availability (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                summary_id TEXT NOT NULL,
                availability TEXT NOT NULL,
                reason TEXT,
                checked_at INTEGER NOT NULL
             );
             CREATE TABLE lcm_summary_convergence_dirty_raw (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                store_id INTEGER NOT NULL,
                rewind_frontier_store_id INTEGER NOT NULL,
                PRIMARY KEY(provider, session_id, store_id)
             );
             CREATE TABLE lcm_summary_convergence_invalidation_work (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                raw_store_id INTEGER NOT NULL,
                source_kind TEXT NOT NULL,
                source_id TEXT NOT NULL,
                depth INTEGER NOT NULL,
                after_node_id TEXT NOT NULL DEFAULT '',
                state TEXT NOT NULL DEFAULT 'pending',
                PRIMARY KEY(provider, session_id, raw_store_id, source_kind, source_id)
             );
             INSERT INTO session_temporal_generations(session_id, generation, state)
             VALUES ('diamond', 1, 'active');
             INSERT INTO lcm_summary_convergence_dirty_raw(
                 provider, session_id, store_id, rewind_frontier_store_id
             ) VALUES ('cursor', 'diamond', 100, 99);
             INSERT INTO lcm_summary_sources(node_id, source_kind, source_id) VALUES
                 ('a', 'raw_message', '100'),
                 ('d', 'raw_message', '100'),
                 ('b', 'summary_node', 'a'),
                 ('d', 'summary_node', 'b'),
                 ('e', 'summary_node', 'd'),
                 ('z', 'raw_message', '200');
             INSERT INTO lcm_summary_nodes(
                 node_id, provider, conversation_id, session_id, depth,
                 summary_text, summary_hash, summary_token_count, source_token_count, created_at
             ) VALUES
                 ('a', 'cursor', 'diamond', 'diamond', 0, 'a', 'a-hash', 1, 1, 1),
                 ('b', 'cursor', 'diamond', 'diamond', 1, 'b', 'b-hash', 1, 1, 2),
                 ('d', 'cursor', 'diamond', 'diamond', 2, 'd', 'd-hash', 1, 1, 3),
                 ('e', 'cursor', 'diamond', 'diamond', 3, 'e', 'e-hash', 1, 1, 4),
                 ('z', 'cursor', 'diamond', 'diamond', 0, 'unrelated',
                  'c2703a7ddf6c74b39505339af20dd6dd4f0794720e038b78ba395600c72417d4',
                  1, 1, 5);
             INSERT INTO session_summary_availability(
                 session_id, generation, summary_id, availability, reason, checked_at
             ) VALUES
                 ('diamond', 1, 'a', 'available', NULL, 0),
                 ('diamond', 1, 'b', 'available', NULL, 0),
                 ('diamond', 1, 'd', 'available', NULL, 0),
                 ('diamond', 1, 'e', 'available', NULL, 0),
                 ('diamond', 1, 'z', 'available', NULL, 0);",
        )
        .await
        .unwrap();

        let mut work_count = 0_usize;
        let mut stale_count = 0_usize;
        let mut pages = Vec::new();
        let first_page =
            invalidate_raw_summary_revision(&conn, "cursor", "diamond", 100, PAGE_LIMIT)
                .await
                .unwrap();
        assert!(first_page.has_more);
        work_count = work_count.saturating_add(first_page.work_count);
        stale_count = stale_count.saturating_add(first_page.stale_summary_count);
        pages.push((
            first_page.work_count,
            first_page.stale_summary_count,
            first_page.has_more,
        ));
        assert!(
            tracedecay_lcm::dag::load_uncondensed_summary_nodes(&conn, "cursor", "diamond")
                .await
                .unwrap()
                .is_empty(),
            "a partial invalidation must hide the entire dirty session"
        );

        for _ in 1..16 {
            let page = invalidate_raw_summary_revision(&conn, "cursor", "diamond", 100, PAGE_LIMIT)
                .await
                .unwrap();
            work_count = work_count.saturating_add(page.work_count);
            stale_count = stale_count.saturating_add(page.stale_summary_count);
            pages.push((page.work_count, page.stale_summary_count, page.has_more));
            if !page.has_more {
                break;
            }
        }

        assert_eq!(stale_count, 4, "unexpected invalidation pages: {pages:?}");
        assert_eq!(
            work_count, 6,
            "the longer path must not retraverse an already drained shortcut node"
        );
        conn.execute(
            "DELETE FROM lcm_summary_convergence_dirty_raw
             WHERE provider = 'cursor' AND session_id = 'diamond' AND store_id = 100",
            (),
        )
        .await
        .unwrap();
        let replay =
            tracedecay_lcm::dag::load_uncondensed_summary_nodes(&conn, "cursor", "diamond")
                .await
                .unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].node.node_id, "z");
    }
}

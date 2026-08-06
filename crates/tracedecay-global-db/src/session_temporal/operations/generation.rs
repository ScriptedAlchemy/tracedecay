use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::json;
use tracedecay_runtime_core::db::engine::{Executor, params};

use tracedecay_sessions::runtime::lcm::types::{LcmError, LcmImmutableSummaryPublication};

use super::PUBLICATION_ROUTE;
use crate::session_temporal::relations::{SessionRelationProjection, SummarySourceRef};

const MAX_LINEAGE_DEPTH: usize = 64;
const MAX_LINEAGE_NODES: usize = 4_096;

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
        let tracedecay_sessions::runtime::lcm::types::LcmSourceRef::SummaryNode { node_id } =
            source
        else {
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

pub(super) async fn validate_current_predecessor(
    conn: &impl Executor,
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
            "SELECT summary_id, publication_json
             FROM session_summary_nodes
             WHERE session_id = ?1
             ORDER BY created_at, summary_id",
            params![publication.draft.session_id.as_str()],
        )
        .await?;
    let mut current_for_identity = Vec::new();
    while let Some(row) = matching.next().await? {
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

pub(super) async fn publish_candidate_generation(
    conn: &impl Executor,
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
    // An aggregate SELECT with COALESCE always returns exactly one row.
    #[allow(clippy::unwrap_used)]
    let max_generation: i64 = max_rows.next().await?.unwrap().get(0)?;
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
    conn: &impl Executor,
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
    conn: &impl Executor,
    session_id: &str,
    active: i64,
    candidate: i64,
) -> Result<(), LcmError> {
    const COPIES: &[&str] = &[
        "INSERT INTO session_turns (
            session_id, generation, turn_id, ordinal, grouping_provenance, created_at
         )
         SELECT session_id, ?2, turn_id, ordinal, grouping_provenance, created_at
         FROM session_turns WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_threads (
            session_id, generation, thread_id, grouping_provenance, created_at
         )
         SELECT session_id, ?2, thread_id, grouping_provenance, created_at
         FROM session_threads WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_agents (
            session_id, generation, agent_id, agent_json, created_at
         )
         SELECT session_id, ?2, agent_id, agent_json, created_at
         FROM session_agents WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            source_provider, projection_output_ordinal, retrieval_anchor_id, thread_id,
            thread_grouping_json, turn_id, turn_grouping_json, message_id,
            agent_id, role, knowledge_at, valid_time_json, evidence_json,
            sanitized_content_digest, sanitized_content_bytes,
            snippet_text, index_text
         )
         SELECT session_id, ?2, occurrence_id, source_observation_id,
                source_provider, projection_output_ordinal, retrieval_anchor_id, thread_id,
                thread_grouping_json, turn_id, turn_grouping_json, message_id,
                agent_id, role, knowledge_at, valid_time_json, evidence_json,
                sanitized_content_digest, sanitized_content_bytes,
                snippet_text, index_text
         FROM session_occurrences WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_turn_members (
            session_id, generation, turn_id, occurrence_id, ordinal
         )
         SELECT session_id, ?2, turn_id, occurrence_id, ordinal
         FROM session_turn_members WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_assertions (
            session_id, generation, assertion_id, assertion_kind,
            subject_anchor_id, object_anchor_id, knowledge_at,
            valid_time_json, evidence_json
         )
         SELECT session_id, ?2, assertion_id, assertion_kind,
                subject_anchor_id, object_anchor_id, knowledge_at,
                valid_time_json, evidence_json
         FROM session_assertions WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_assertion_supersession (
            session_id, generation, superseded_assertion_id,
            superseding_assertion_id, created_at
         )
         SELECT session_id, ?2, superseded_assertion_id,
                superseding_assertion_id, created_at
         FROM session_assertion_supersession
         WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_current_entities (
            session_id, generation, entity_kind, entity_id,
            current_assertion_id, current_occurrence_id, coverage_json
         )
         SELECT session_id, ?2, entity_kind, entity_id,
                current_assertion_id, current_occurrence_id, coverage_json
         FROM session_current_entities WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_derived_evidence (
            session_id, generation, evidence_kind, evidence_id,
            retrieval_anchor_id, thread_id,
            first_occurrence_id, last_occurrence_id,
            algorithm_version, configuration_digest,
            member_count, member_digest, evidence_json
         )
         SELECT session_id, ?2, evidence_kind, evidence_id,
                retrieval_anchor_id, thread_id,
                first_occurrence_id, last_occurrence_id,
                algorithm_version, configuration_digest,
                member_count, member_digest, evidence_json
         FROM session_derived_evidence WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_derived_evidence_members (
            session_id, generation, evidence_kind, evidence_id,
            ordinal, occurrence_id, member_role
         )
         SELECT session_id, ?2, evidence_kind, evidence_id,
                ordinal, occurrence_id, member_role
         FROM session_derived_evidence_members
         WHERE session_id = ?1 AND generation = ?3",
    ];
    for sql in COPIES {
        conn.execute(sql, params![session_id, candidate, active])
            .await?;
    }
    Ok(())
}

pub(super) async fn active_generation(
    conn: &impl Executor,
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
    conn: &impl Executor,
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

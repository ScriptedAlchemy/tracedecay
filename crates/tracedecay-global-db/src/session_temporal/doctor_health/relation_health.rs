use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{SessionId, SessionProjectionGenerationV1};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_runtime_core::db::engine::{
    Error as EngineError, QueryExecutor, Result as EngineResult,
};

use crate::RegisteredGlobalDb;

use super::{
    SessionTemporalHealthFinding, SessionTemporalHealthFindingKind, SessionTemporalHealthReport,
    SessionTemporalHealthStatus, finding, merge_finding,
};
use crate::session_temporal::relations::{
    SessionRelationError, SessionRelationProjection, SummaryRelationNode, SummarySourceRef,
};

const MAX_RELATION_HEALTH_PROJECTIONS: usize = 256;
const RELATION_HEALTH_QUERY_LIMIT: i64 = MAX_RELATION_HEALTH_PROJECTIONS as i64 + 1;
const MAX_RELATION_HEALTH_ENTITIES: usize = 100_000;
const MAX_RELATION_HEALTH_RELATIONS: usize = 100_000;

impl RegisteredGlobalDb {
    pub(super) async fn with_relation_graph_health(
        &self,
        mut report: SessionTemporalHealthReport,
    ) -> SessionTemporalHealthReport {
        if report.status != SessionTemporalHealthStatus::Complete {
            return report;
        }
        let (scope, store) = match self.session_relation_store() {
            Ok(authority) => authority,
            Err(_) => {
                report.status = SessionTemporalHealthStatus::Partial;
                report.findings.push(finding(
                    SessionTemporalHealthFindingKind::RelationGraphUnavailable,
                    1,
                ));
                return report;
            }
        };
        let snapshot = match self.read_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(_) => {
                report.status = SessionTemporalHealthStatus::Partial;
                report.findings.push(finding(
                    SessionTemporalHealthFindingKind::RelationGraphUnavailable,
                    1,
                ));
                return report;
            }
        };
        let mut rows = match snapshot
            .query(
                "SELECT session_id, generation
                 FROM session_temporal_generations
                 WHERE state = 'active'
                 ORDER BY session_id, generation
                 LIMIT ?1",
                [RELATION_HEALTH_QUERY_LIMIT],
            )
            .await
        {
            Ok(rows) => rows,
            Err(_) => {
                report.status = SessionTemporalHealthStatus::Partial;
                report.findings.push(finding(
                    SessionTemporalHealthFindingKind::RelationGraphUnavailable,
                    1,
                ));
                return report;
            }
        };
        let mut projections = Vec::new();
        loop {
            let row = match rows.next().await {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(_) => {
                    report.status = SessionTemporalHealthStatus::Partial;
                    merge_finding(
                        &mut report.findings,
                        SessionTemporalHealthFindingKind::RelationGraphUnavailable,
                        1,
                    );
                    break;
                }
            };
            if projections.len() == MAX_RELATION_HEALTH_PROJECTIONS {
                report.status = SessionTemporalHealthStatus::Partial;
                merge_finding(
                    &mut report.findings,
                    SessionTemporalHealthFindingKind::RelationGraphUnavailable,
                    1,
                );
                break;
            }
            let Ok(session) = row.get::<String>(0) else {
                report.status = SessionTemporalHealthStatus::Partial;
                merge_finding(
                    &mut report.findings,
                    SessionTemporalHealthFindingKind::RelationGraphUnavailable,
                    1,
                );
                continue;
            };
            let Ok(generation) = row.get::<i64>(1) else {
                report.status = SessionTemporalHealthStatus::Partial;
                merge_finding(
                    &mut report.findings,
                    SessionTemporalHealthFindingKind::RelationGraphUnavailable,
                    1,
                );
                continue;
            };
            let Ok(session_id) = SessionId::new(session) else {
                merge_finding(
                    &mut report.findings,
                    SessionTemporalHealthFindingKind::RelationGraphCorruption,
                    1,
                );
                continue;
            };
            let Ok(generation) = u64::try_from(generation)
                .ok()
                .and_then(|value| SessionProjectionGenerationV1::new(value).ok())
                .ok_or(())
            else {
                merge_finding(
                    &mut report.findings,
                    SessionTemporalHealthFindingKind::RelationGraphCorruption,
                    1,
                );
                continue;
            };
            projections.push((session_id, generation));
        }
        for (session_id, generation) in projections {
            let projection = match store.load_projection(
                scope,
                &session_id,
                generation.value(),
                MAX_RELATION_HEALTH_ENTITIES,
                MAX_RELATION_HEALTH_RELATIONS,
                Arc::new(NeverCancelled),
            ) {
                Ok(projection) => projection,
                Err(error) => {
                    let (kind, unavailable) = relation_error_finding(&error);
                    if unavailable {
                        report.status = SessionTemporalHealthStatus::Partial;
                    }
                    merge_finding(&mut report.findings, kind, 1);
                    continue;
                }
            };
            match stale_summary_closure_count(&snapshot, &projection).await {
                Ok(count) if count > 0 => merge_finding(
                    &mut report.findings,
                    SessionTemporalHealthFindingKind::StaleSummaryClosure,
                    count,
                ),
                Ok(_) => {}
                Err(_) => {
                    report.status = SessionTemporalHealthStatus::Partial;
                    merge_finding(
                        &mut report.findings,
                        SessionTemporalHealthFindingKind::RelationGraphUnavailable,
                        1,
                    );
                }
            }
        }
        report
            .findings
            .sort_by_key(SessionTemporalHealthFinding::kind);
        report
    }
}

fn relation_error_finding(
    error: &SessionRelationError,
) -> (SessionTemporalHealthFindingKind, bool) {
    match error {
        SessionRelationError::Cycle => {
            (SessionTemporalHealthFindingKind::RelationGraphCycle, false)
        }
        SessionRelationError::Corrupt | SessionRelationError::Invalid => (
            SessionTemporalHealthFindingKind::RelationGraphCorruption,
            false,
        ),
        _ => (
            SessionTemporalHealthFindingKind::RelationGraphUnavailable,
            true,
        ),
    }
}

async fn stale_summary_closure_count(
    conn: &impl QueryExecutor,
    projection: &SessionRelationProjection,
) -> EngineResult<u64> {
    let summaries = projection
        .summaries
        .iter()
        .map(|summary| (summary.summary_id.as_str(), summary))
        .collect::<BTreeMap<_, _>>();
    let expected_stale = expected_stale_summary_ids(&projection.summaries);
    if expected_stale
        .iter()
        .any(|summary_id| !summaries.contains_key(summary_id))
    {
        return Err(EngineError::Runtime(
            "relation graph stale closure references an absent summary".to_owned(),
        ));
    }
    let availability_limit = projection
        .summaries
        .len()
        .checked_add(1)
        .ok_or_else(|| EngineError::Runtime("summary health bound overflowed".to_owned()))
        .and_then(|limit| {
            i64::try_from(limit).map_err(|error| EngineError::Runtime(error.to_string()))
        })?;
    let mut rows = conn
        .query(
            "SELECT summary_id, availability
             FROM session_summary_availability
             WHERE session_id = ?1 AND generation = ?2
             ORDER BY summary_id
             LIMIT ?3",
            tracedecay_runtime_core::db::engine::params![
                projection.session_id.as_str(),
                i64::try_from(projection.generation)
                    .map_err(|error| EngineError::Runtime(error.to_string()))?,
                availability_limit
            ],
        )
        .await?;
    let mut availability = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        if availability.len() == projection.summaries.len() {
            return Err(EngineError::Runtime(
                "session summary availability exceeded relation projection bounds".to_owned(),
            ));
        }
        availability.insert(row.get::<String>(0)?, row.get::<String>(1)?);
    }
    stale_summary_difference_count(&expected_stale, &availability)
}

fn expected_stale_summary_ids(summaries: &[SummaryRelationNode]) -> BTreeSet<&str> {
    let mut reverse_dependencies = BTreeMap::<&str, Vec<&str>>::new();
    for summary in summaries {
        for source in &summary.sources {
            if let SummarySourceRef::Summary { summary_id } = source {
                reverse_dependencies
                    .entry(summary_id.as_str())
                    .or_default()
                    .push(summary.summary_id.as_str());
            }
        }
    }
    let mut expected_stale = summaries
        .iter()
        .filter_map(|summary| summary.predecessor_summary_id.as_deref())
        .collect::<BTreeSet<_>>();
    let mut pending = expected_stale.iter().copied().collect::<Vec<_>>();
    while let Some(summary_id) = pending.pop() {
        for dependent in reverse_dependencies.get(summary_id).into_iter().flatten() {
            if expected_stale.insert(*dependent) {
                pending.push(*dependent);
            }
        }
    }
    expected_stale
}

fn stale_summary_difference_count(
    expected_stale: &BTreeSet<&str>,
    availability: &BTreeMap<String, String>,
) -> EngineResult<u64> {
    let missing = expected_stale
        .iter()
        .filter(|summary_id| availability.get(**summary_id).map(String::as_str) != Some("stale"))
        .count();
    let spurious = availability
        .iter()
        .filter(|(summary_id, state)| {
            state.as_str() == "stale" && !expected_stale.contains(summary_id.as_str())
        })
        .count();
    let difference = missing.checked_add(spurious).ok_or_else(|| {
        EngineError::Runtime("stale summary difference count overflowed".to_owned())
    })?;
    u64::try_from(difference).map_err(|error| EngineError::Runtime(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_summary_health_uses_the_exact_transitive_closure() {
        let summaries = vec![
            SummaryRelationNode {
                summary_id: "old".to_owned(),
                sources: Vec::new(),
                predecessor_summary_id: None,
            },
            SummaryRelationNode {
                summary_id: "successor".to_owned(),
                sources: Vec::new(),
                predecessor_summary_id: Some("old".to_owned()),
            },
            SummaryRelationNode {
                summary_id: "dependent".to_owned(),
                sources: vec![SummarySourceRef::Summary {
                    summary_id: "old".to_owned(),
                }],
                predecessor_summary_id: None,
            },
            SummaryRelationNode {
                summary_id: "transitive".to_owned(),
                sources: vec![SummarySourceRef::Summary {
                    summary_id: "dependent".to_owned(),
                }],
                predecessor_summary_id: None,
            },
        ];

        assert_eq!(
            expected_stale_summary_ids(&summaries),
            BTreeSet::from(["dependent", "old", "transitive"])
        );
    }

    #[test]
    fn stale_summary_health_detects_spurious_stale_state_without_a_supersession() {
        let availability = BTreeMap::from([("current".to_owned(), "stale".to_owned())]);

        assert_eq!(
            stale_summary_difference_count(&BTreeSet::new(), &availability).unwrap(),
            1
        );
    }

    #[test]
    fn relation_cycle_is_not_collapsed_into_generic_corruption() {
        assert_eq!(
            relation_error_finding(&SessionRelationError::Cycle),
            (SessionTemporalHealthFindingKind::RelationGraphCycle, false)
        );
    }
}

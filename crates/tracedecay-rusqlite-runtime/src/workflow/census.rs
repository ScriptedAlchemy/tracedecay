use tracedecay_application::{
    WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1, WorkflowActiveRunRecoveryCursorV1,
    WorkflowFanOutCensusBackfillPageV1, WorkflowFanOutCensusError,
    WorkflowFanOutCensusObservationV1, WorkflowFanOutCensusPersistOutcomeV1,
    WorkflowFanOutCensusStoragePort,
};
use tracedecay_domain::{
    ObservabilityTerminalResultV1, RunId, WorkAuthority, WorkflowFanOutCensusV1, WorkflowRunEvent,
    WorkflowRunProjection, WorkflowRunStatus, canonical_sha256,
};

use super::{
    ExactSqlTransaction, ExactSqlValue, WorkflowSqliteAuthority, decode_json, encode_json,
    execute_tx, execute_tx_changed, query_tx, sql_text,
};

fn unavailable<E>(_: E) -> WorkflowFanOutCensusError {
    WorkflowFanOutCensusError::Unavailable
}

fn decode_census(
    payload: &str,
    stored_digest: &str,
) -> Result<WorkflowFanOutCensusV1, WorkflowFanOutCensusError> {
    let census: WorkflowFanOutCensusV1 =
        decode_json(payload).map_err(|_| WorkflowFanOutCensusError::InvalidHistory)?;
    census
        .validate()
        .map_err(|_| WorkflowFanOutCensusError::InvalidHistory)?;
    let digest =
        canonical_sha256(&census).map_err(|_| WorkflowFanOutCensusError::InvalidHistory)?;
    if digest.as_str() != stored_digest {
        return Err(WorkflowFanOutCensusError::InvalidHistory);
    }
    Ok(census)
}

fn latest_tx(
    transaction: &ExactSqlTransaction,
    run_id: &RunId,
) -> Result<Option<WorkflowFanOutCensusV1>, WorkflowFanOutCensusError> {
    let rows = query_tx(
        transaction,
        "SELECT census_payload, census_digest
         FROM workflow_fan_out_census_journal
         WHERE run_id = ?1
         ORDER BY workflow_sequence DESC LIMIT 1",
        vec![ExactSqlValue::Text(run_id.as_str().to_owned())],
    )
    .map_err(unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            let payload =
                sql_text(&row.values, 0).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            let digest =
                sql_text(&row.values, 1).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            decode_census(payload, digest)
        })
        .transpose()
}

fn before_tx(
    transaction: &ExactSqlTransaction,
    run_id: &RunId,
    workflow_sequence: u64,
) -> Result<Option<WorkflowFanOutCensusV1>, WorkflowFanOutCensusError> {
    let sequence =
        i64::try_from(workflow_sequence).map_err(|_| WorkflowFanOutCensusError::InvalidInput)?;
    let rows = query_tx(
        transaction,
        "SELECT census_payload, census_digest
         FROM workflow_fan_out_census_journal
         WHERE run_id = ?1 AND workflow_sequence < ?2
         ORDER BY workflow_sequence DESC LIMIT 1",
        vec![
            ExactSqlValue::Text(run_id.as_str().to_owned()),
            ExactSqlValue::Integer(sequence),
        ],
    )
    .map_err(unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            let payload =
                sql_text(&row.values, 0).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            let digest =
                sql_text(&row.values, 1).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            decode_census(payload, digest)
        })
        .transpose()
}

fn projection_through_tx(
    transaction: &ExactSqlTransaction,
    run_id: &RunId,
    sequence: u64,
) -> Result<WorkflowRunProjection, WorkflowFanOutCensusError> {
    let expected_sequence = sequence;
    let sequence =
        i64::try_from(expected_sequence).map_err(|_| WorkflowFanOutCensusError::InvalidInput)?;
    let rows = query_tx(
        transaction,
        "SELECT event_payload, event_digest FROM workflow_run_journal
         WHERE run_id = ?1 AND sequence <= ?2 ORDER BY sequence",
        vec![
            ExactSqlValue::Text(run_id.as_str().to_owned()),
            ExactSqlValue::Integer(sequence),
        ],
    )
    .map_err(unavailable)?;
    let history = rows
        .rows
        .iter()
        .map(|row| {
            let payload =
                sql_text(&row.values, 0).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            let stored_digest =
                sql_text(&row.values, 1).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            let event: WorkflowRunEvent =
                decode_json(payload).map_err(|_| WorkflowFanOutCensusError::InvalidHistory)?;
            let digest =
                canonical_sha256(&event).map_err(|_| WorkflowFanOutCensusError::InvalidHistory)?;
            if digest.as_str() != stored_digest {
                return Err(WorkflowFanOutCensusError::InvalidHistory);
            }
            Ok(event)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let projection = WorkflowRunProjection::rebuild(&history)
        .map_err(|_| WorkflowFanOutCensusError::InvalidHistory)?;
    if projection.sequence() != expected_sequence {
        return Err(WorkflowFanOutCensusError::InvalidHistory);
    }
    Ok(projection)
}

impl WorkflowFanOutCensusStoragePort for WorkflowSqliteAuthority {
    fn latest_census(
        &self,
        run_id: &RunId,
    ) -> Result<Option<WorkflowFanOutCensusV1>, WorkflowFanOutCensusError> {
        let transaction = self.handle().begin_immediate().map_err(unavailable)?;
        let result = latest_tx(&transaction, run_id);
        let _ = transaction.rollback();
        result
    }

    fn census_before(
        &self,
        run_id: &RunId,
        workflow_sequence: u64,
    ) -> Result<Option<WorkflowFanOutCensusV1>, WorkflowFanOutCensusError> {
        let transaction = self.handle().begin_immediate().map_err(unavailable)?;
        let result = before_tx(&transaction, run_id, workflow_sequence);
        let _ = transaction.rollback();
        result
    }

    fn persist_census(
        &self,
        census: &WorkflowFanOutCensusV1,
    ) -> Result<WorkflowFanOutCensusPersistOutcomeV1, WorkflowFanOutCensusError> {
        census
            .validate()
            .map_err(|_| WorkflowFanOutCensusError::InvalidInput)?;
        let payload = encode_json(census).map_err(unavailable)?;
        let digest = canonical_sha256(census).map_err(unavailable)?;
        let workflow_sequence = i64::try_from(census.workflow_sequence)
            .map_err(|_| WorkflowFanOutCensusError::InvalidInput)?;
        let transaction = self.handle().begin_immediate().map_err(unavailable)?;
        let projection =
            match projection_through_tx(&transaction, &census.run_id, census.workflow_sequence) {
                Ok(projection) => projection,
                Err(error) => {
                    let _ = transaction.rollback();
                    return Err(error);
                }
            };
        if projection.pinned_topology_digest() != &census.topology_digest
            || projection.pinned_provider_registry_digest() != &census.provider_registry_digest
            || projection
                .history()
                .last()
                .is_none_or(|event| event.occurred_at() > census.observed_at)
        {
            let _ = transaction.rollback();
            return Err(WorkflowFanOutCensusError::Conflict);
        }
        let requested = projection
            .fan_out_plans()
            .values()
            .map(|plan| plan.children.len())
            .sum::<usize>();
        if requested == 0 || census.requested_width.known() != u16::try_from(requested).ok() {
            let _ = transaction.rollback();
            return Err(WorkflowFanOutCensusError::Conflict);
        }
        let existing = query_tx(
            &transaction,
            "SELECT census_digest FROM workflow_fan_out_census_journal
             WHERE run_id = ?1 AND workflow_sequence = ?2",
            vec![
                ExactSqlValue::Text(census.run_id.as_str().to_owned()),
                ExactSqlValue::Integer(workflow_sequence),
            ],
        )
        .map_err(unavailable)?;
        if let Some(row) = existing.rows.first() {
            let outcome = if sql_text(&row.values, 0) == Some(digest.as_str()) {
                Ok(WorkflowFanOutCensusPersistOutcomeV1::Replayed)
            } else {
                Err(WorkflowFanOutCensusError::Conflict)
            };
            let _ = transaction.rollback();
            return outcome;
        }
        let previous = latest_tx(&transaction, &census.run_id)?;
        if let Some(latest) = previous.as_ref() {
            if latest.workflow_sequence > census.workflow_sequence
                || latest.observed_at > census.observed_at
                || census.interval_started_at != latest.observed_at
            {
                let _ = transaction.rollback();
                return Err(WorkflowFanOutCensusError::Conflict);
            }
        } else if census.interval_started_at != census.observed_at {
            let _ = transaction.rollback();
            return Err(WorkflowFanOutCensusError::Conflict);
        }
        let observability_settled = i64::from(
            previous
                .as_ref()
                .is_none_or(|prior| prior.observed_at >= census.observed_at)
                || census.execution_topology_sample().is_none(),
        );
        execute_tx(
            &transaction,
            "INSERT INTO workflow_fan_out_census_journal (
                 run_id, workflow_sequence, observed_at, census_payload, census_digest,
                 observability_settled
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            vec![
                ExactSqlValue::Text(census.run_id.as_str().to_owned()),
                ExactSqlValue::Integer(workflow_sequence),
                ExactSqlValue::Integer(census.observed_at.0),
                ExactSqlValue::Text(payload),
                ExactSqlValue::Text(digest.as_str().to_owned()),
                ExactSqlValue::Integer(observability_settled),
            ],
        )
        .map_err(unavailable)?;
        transaction
            .commit()
            .map(|_| WorkflowFanOutCensusPersistOutcomeV1::Persisted)
            .map_err(unavailable)
    }

    fn pending_census_observations(
        &self,
        limit: u16,
    ) -> Result<Vec<WorkflowFanOutCensusObservationV1>, WorkflowFanOutCensusError> {
        if limit == 0 || limit > 256 {
            return Err(WorkflowFanOutCensusError::InvalidInput);
        }
        let transaction = self.handle().begin_immediate().map_err(unavailable)?;
        let rows = query_tx(
            &transaction,
            "SELECT census_payload, census_digest
             FROM workflow_fan_out_census_journal
             WHERE observability_settled = 0
             ORDER BY observed_at, run_id, workflow_sequence
             LIMIT ?1",
            vec![ExactSqlValue::Integer(i64::from(limit))],
        )
        .map_err(unavailable)?;
        let mut observations = Vec::with_capacity(rows.rows.len());
        for row in rows.rows {
            let payload =
                sql_text(&row.values, 0).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            let digest =
                sql_text(&row.values, 1).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            let census = decode_census(payload, digest)?;
            if census.execution_topology_sample().is_none() {
                let _ = transaction.rollback();
                return Err(WorkflowFanOutCensusError::InvalidHistory);
            }
            let previous = before_tx(&transaction, &census.run_id, census.workflow_sequence)?
                .ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
            let projection =
                projection_through_tx(&transaction, &census.run_id, census.workflow_sequence)?;
            let terminal = match projection.status() {
                WorkflowRunStatus::Completed => Some(ObservabilityTerminalResultV1::Succeeded),
                WorkflowRunStatus::Failed => Some(ObservabilityTerminalResultV1::Failed),
                WorkflowRunStatus::Cancelled => Some(ObservabilityTerminalResultV1::Cancelled),
                WorkflowRunStatus::Running
                | WorkflowRunStatus::Paused
                | WorkflowRunStatus::Cancelling => None,
            };
            observations.push(WorkflowFanOutCensusObservationV1 {
                census,
                previous_observed_at: previous.observed_at,
                terminal,
            });
        }
        let _ = transaction.rollback();
        Ok(observations)
    }

    fn census_backfill_projection_page(
        &self,
        authority: &WorkAuthority,
        after: Option<&WorkflowActiveRunRecoveryCursorV1>,
    ) -> Result<WorkflowFanOutCensusBackfillPageV1, WorkflowFanOutCensusError> {
        let transaction = self.handle().begin_immediate().map_err(unavailable)?;
        let page_limit = i64::try_from(WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1 + 1)
            .map_err(|_| WorkflowFanOutCensusError::Unavailable)?;
        let rows = match after {
            Some(cursor) => query_tx(
                &transaction,
                "SELECT journal.run_id, MAX(journal.sequence),
                        (SELECT MAX(census.workflow_sequence)
                         FROM workflow_fan_out_census_journal AS census
                         WHERE census.run_id = journal.run_id)
                 FROM workflow_run_journal AS journal
                 WHERE journal.run_id > ?1
                 GROUP BY journal.run_id ORDER BY journal.run_id LIMIT ?2",
                vec![
                    ExactSqlValue::Text(cursor.after_run_id.as_str().to_owned()),
                    ExactSqlValue::Integer(page_limit),
                ],
            ),
            None => query_tx(
                &transaction,
                "SELECT journal.run_id, MAX(journal.sequence),
                        (SELECT MAX(census.workflow_sequence)
                         FROM workflow_fan_out_census_journal AS census
                         WHERE census.run_id = journal.run_id)
                 FROM workflow_run_journal AS journal
                 GROUP BY journal.run_id ORDER BY journal.run_id LIMIT ?1",
                vec![ExactSqlValue::Integer(page_limit)],
            ),
        }
        .map_err(unavailable)?;
        let heads = rows
            .rows
            .iter()
            .map(|row| {
                let value =
                    sql_text(&row.values, 0).ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
                let run_id = RunId::new(value.to_owned())
                    .map_err(|_| WorkflowFanOutCensusError::InvalidHistory)?;
                let workflow_sequence = row
                    .values
                    .get(1)
                    .and_then(|value| match value {
                        ExactSqlValue::Integer(value) => u64::try_from(*value).ok(),
                        _ => None,
                    })
                    .ok_or(WorkflowFanOutCensusError::InvalidHistory)?;
                let census_sequence = match row.values.get(2) {
                    Some(ExactSqlValue::Integer(value)) => Some(
                        u64::try_from(*value)
                            .map_err(|_| WorkflowFanOutCensusError::InvalidHistory)?,
                    ),
                    Some(ExactSqlValue::Null) => None,
                    _ => return Err(WorkflowFanOutCensusError::InvalidHistory),
                };
                Ok((run_id, workflow_sequence, census_sequence))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let page_heads = heads
            .iter()
            .take(WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1)
            .cloned()
            .collect::<Vec<_>>();
        let continuation = (heads.len() > WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1).then(|| {
            WorkflowActiveRunRecoveryCursorV1 {
                after_run_id: page_heads[WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1 - 1]
                    .0
                    .clone(),
            }
        });
        let mut projections = Vec::new();
        for (run_id, workflow_sequence, census_sequence) in page_heads {
            if census_sequence.is_some_and(|sequence| sequence > workflow_sequence) {
                let _ = transaction.rollback();
                return Err(WorkflowFanOutCensusError::InvalidHistory);
            }
            if census_sequence == Some(workflow_sequence) {
                continue;
            }
            let projection = projection_through_tx(&transaction, &run_id, workflow_sequence)?;
            if projection.fan_out_plans().is_empty()
                || !projection
                    .fan_out_plans()
                    .values()
                    .all(|plan| &plan.authority == authority)
            {
                continue;
            }
            projections.push(projection);
        }
        let _ = transaction.rollback();
        Ok(WorkflowFanOutCensusBackfillPageV1 {
            projections,
            continuation,
        })
    }

    fn mark_census_observability_durable(
        &self,
        census: &WorkflowFanOutCensusV1,
    ) -> Result<(), WorkflowFanOutCensusError> {
        let sequence = i64::try_from(census.workflow_sequence)
            .map_err(|_| WorkflowFanOutCensusError::InvalidInput)?;
        let digest = canonical_sha256(census).map_err(unavailable)?;
        let transaction = self.handle().begin_immediate().map_err(unavailable)?;
        let changed = execute_tx_changed(
            &transaction,
            "UPDATE workflow_fan_out_census_journal
             SET observability_settled = 1
             WHERE run_id = ?1 AND workflow_sequence = ?2
               AND census_digest = ?3 AND observability_settled = 0",
            vec![
                ExactSqlValue::Text(census.run_id.as_str().to_owned()),
                ExactSqlValue::Integer(sequence),
                ExactSqlValue::Text(digest.as_str().to_owned()),
            ],
        )
        .map_err(unavailable)?;
        if changed > 1 {
            let _ = transaction.rollback();
            return Err(WorkflowFanOutCensusError::InvalidHistory);
        }
        if changed == 0 {
            let rows = query_tx(
                &transaction,
                "SELECT census_digest, observability_settled
                 FROM workflow_fan_out_census_journal
                 WHERE run_id = ?1 AND workflow_sequence = ?2",
                vec![
                    ExactSqlValue::Text(census.run_id.as_str().to_owned()),
                    ExactSqlValue::Integer(sequence),
                ],
            )
            .map_err(unavailable)?;
            let Some(row) = rows.rows.first() else {
                let _ = transaction.rollback();
                return Err(WorkflowFanOutCensusError::InvalidHistory);
            };
            if sql_text(&row.values, 0) != Some(digest.as_str())
                || !matches!(row.values.get(1), Some(ExactSqlValue::Integer(1)))
            {
                let _ = transaction.rollback();
                return Err(WorkflowFanOutCensusError::Conflict);
            }
        }
        transaction.commit().map(|_| ()).map_err(unavailable)
    }
}

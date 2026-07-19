use libsql::params;

use crate::global_db::GlobalDbReadSnapshot;
use crate::query::temporal::candidates::CandidatePlan;
use crate::query::temporal::ports::{
    CandidatePageSink, MeasuredTemporalValue, PageRequest, PageStatus, PortFuture,
    TemporalExecutionSnapshot, TemporalPortError, TemporalReadPort, TemporalRecordPageSink,
    TemporalRetrievalScope,
};
use crate::query::temporal::ranking::RankingCandidate;

mod candidates;
mod cursors;
mod queries;
mod records;
mod rows;
#[cfg(test)]
mod tests;

use candidates::*;
use cursors::*;
use records::*;
use rows::*;

pub(crate) const CANDIDATE_OPERATION: &str = "read temporal candidates";
pub(crate) const RECORD_OPERATION: &str = "read temporal records";
pub(crate) const SNAPSHOT_OPERATION: &str = "validate temporal read snapshot";
pub(crate) const MIN_CURSOR_CAPACITY: usize = 96;
pub(crate) const MAX_SUMMARY_SOURCES_PER_RECORD: usize = 256;

/// Borrowed read-only adapter over one authoritative database snapshot.
pub struct GlobalDbTemporalReadPort<'a> {
    read: &'a GlobalDbReadSnapshot,
}

impl<'a> GlobalDbTemporalReadPort<'a> {
    pub const fn new(read: &'a GlobalDbReadSnapshot) -> Self {
        Self { read }
    }

    async fn validate_snapshot(
        &self,
        snapshot: &TemporalExecutionSnapshot,
    ) -> Result<(), TemporalPortError> {
        let control = snapshot.request().execution_control();
        control.checkpoint()?;
        if !snapshot.has_authoritative_participant_manifest() {
            if matches!(
                snapshot.retrieval_scope(),
                TemporalRetrievalScope::AllSessionsInAuthorizedRoot
            ) {
                return Err(TemporalPortError::UnauthorizedSnapshot);
            }
            let generation = i64::try_from(snapshot.watermarks().generation)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let mut rows = self
                .read
                .query(
                    "SELECT state, frozen_watermarks_json
                     FROM session_temporal_generations
                     WHERE session_id = ?1 AND generation = ?2
                     LIMIT 2",
                    (snapshot.request().session_id().as_str(), generation),
                )
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let row = rows
                .next()
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?
                .ok_or_else(|| read_message(SNAPSHOT_OPERATION, "frozen generation is missing"))?;
            let state: String = row
                .get(0)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let encoded: String = row
                .get(1)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            if rows
                .next()
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?
                .is_some()
            {
                return Err(read_message(
                    SNAPSHOT_OPERATION,
                    "frozen generation is not unique",
                ));
            }
            let frozen: FrozenWatermarksWire = serde_json::from_str(&encoded)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let watermarks = snapshot.watermarks();
            if state != "active"
                || frozen.active_generation > watermarks.generation
                || frozen.source_frontier != watermarks.source
                || frozen.projection_frontier != watermarks.projection
                || frozen.summary_frontier != watermarks.summary
                || frozen.cursor_key.as_ref() != snapshot.cursor_key()
            {
                return Err(read_message(
                    SNAPSHOT_OPERATION,
                    "snapshot does not match the active frozen generation",
                ));
            }
            return control.checkpoint();
        }
        let project_key = snapshot
            .request()
            .authorized_root()
            .ok_or(TemporalPortError::UnauthorizedSnapshot)?
            .project_key();
        for participant in snapshot.participant_manifest().entries() {
            control.checkpoint()?;
            if participant.access()
                != crate::query::temporal::ports::TemporalSourceAccess::Authorized
                || participant.configuration_digest()
                    != snapshot.versions().configuration_digest.as_str()
                || participant.authorization_digest() != snapshot.access_digest().as_str()
            {
                return Err(TemporalPortError::UnauthorizedSnapshot);
            }
            let generation = i64::try_from(participant.generation())
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let mut rows = self
                .read
                .query(
                    "SELECT generation.state, generation.frozen_watermarks_json
                     FROM session_temporal_generations AS generation
                     JOIN sessions AS source
                       ON source.session_id = generation.session_id
                      AND source.provider = ?3
                      AND source.project_key = ?4
                     WHERE generation.session_id = ?1
                       AND generation.generation = ?2
                     LIMIT 2",
                    params![
                        participant.session_id().as_str(),
                        generation,
                        participant.source_id(),
                        project_key
                    ],
                )
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let row = rows
                .next()
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?
                .ok_or_else(|| {
                    read_message(
                        SNAPSHOT_OPERATION,
                        "frozen participant generation is missing",
                    )
                })?;
            let state: String = row
                .get(0)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let encoded: String = row
                .get(1)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            if rows
                .next()
                .await
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?
                .is_some()
            {
                return Err(read_message(
                    SNAPSHOT_OPERATION,
                    "frozen participant generation is not unique",
                ));
            }
            let frozen: FrozenWatermarksWire = serde_json::from_str(&encoded)
                .map_err(|error| read_error(SNAPSHOT_OPERATION, error))?;
            let watermarks = participant.watermarks();
            if state != "active"
                || frozen.active_generation > watermarks.generation
                || frozen.source_frontier != watermarks.source
                || frozen.projection_frontier != watermarks.projection
                || frozen.summary_frontier != watermarks.summary
                || frozen.cursor_key.as_ref() != snapshot.cursor_key()
            {
                return Err(read_message(
                    SNAPSHOT_OPERATION,
                    "snapshot does not match the active participant generation",
                ));
            }
        }
        control.checkpoint()
    }

    async fn produce_candidates(
        &self,
        scope: &TemporalRetrievalScope,
        snapshot: &TemporalExecutionSnapshot,
        plan: &CandidatePlan,
        request: &PageRequest,
        sink: &mut CandidatePageSink<'_>,
    ) -> Result<PageStatus, TemporalPortError> {
        require_snapshot_scope(scope, snapshot)?;
        let bounds = PageBounds::from_request(request)?;
        if bounds.items == 0 || bounds.bytes == 0 {
            return Ok(PageStatus::Complete);
        }
        self.validate_snapshot(snapshot).await?;
        let root_project_key = authorized_root_project_key(scope, snapshot)?;
        let mut cursor = CandidateCursor::decode(request.keyset())?;
        if cursor.clause >= plan.clauses().len() {
            return Ok(PageStatus::Complete);
        }
        let control = snapshot.request().execution_control();
        let mut page_bytes = 0usize;
        let mut clause_queries = 0usize;
        while cursor.clause < plan.clauses().len() {
            control.checkpoint()?;
            clause_queries += 1;
            if clause_queries > bounds.items.saturating_add(1) {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate clause scans",
                });
            }
            let clause = &plan.clauses()[cursor.clause];
            validate_clause(clause, request)?;
            let query_limit = bounds.items.saturating_sub(sink.len()).saturating_add(1);
            let mut rows = query_candidate_clause(
                self.read,
                scope,
                snapshot,
                clause,
                &cursor,
                query_limit,
                request,
                root_project_key,
            )
            .await?;
            let mut extra = false;
            let mut last_emitted = None;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| read_error(CANDIDATE_OPERATION, error))?
            {
                control.checkpoint()?;
                if sink.len() == bounds.items {
                    extra = true;
                    break;
                }
                let candidate = candidate_from_row(&row, clause.channel, scope)?;
                require_candidate_scope(scope, &candidate)?;
                let encoded = candidate.measured_encoded_bytes()?;
                if !fits_bytes(page_bytes, encoded, bounds, request.max_item_bytes()) {
                    if sink.is_empty() {
                        return Err(TemporalPortError::BudgetExceeded {
                            resource: "candidate bytes",
                        });
                    }
                    extra = true;
                    break;
                }
                page_bytes += encoded;
                last_emitted = Some(CandidateCursor {
                    clause: cursor.clause,
                    knowledge_at: candidate.knowledge_at_micros,
                    session_id: candidate.session.clone().unwrap_or_default(),
                    stable_id: candidate.retriever_record_id.clone(),
                });
                sink.push(candidate)?;
            }
            if extra {
                let continuation = last_emitted.unwrap_or(cursor);
                sink.set_continuation_key(continuation.encode(request.max_key_bytes())?)?;
                return Ok(PageStatus::More);
            }
            cursor = CandidateCursor {
                clause: cursor.clause + 1,
                knowledge_at: i64::MAX,
                session_id: String::new(),
                stable_id: String::new(),
            };
            if sink.len() == bounds.items {
                if cursor.clause < plan.clauses().len() {
                    sink.set_continuation_key(cursor.encode(request.max_key_bytes())?)?;
                    return Ok(PageStatus::More);
                }
                return Ok(PageStatus::Complete);
            }
        }
        Ok(PageStatus::Complete)
    }

    async fn produce_records(
        &self,
        scope: &TemporalRetrievalScope,
        snapshot: &TemporalExecutionSnapshot,
        candidates: &[RankingCandidate],
        request: &PageRequest,
        sink: &mut TemporalRecordPageSink<'_>,
    ) -> Result<PageStatus, TemporalPortError> {
        require_snapshot_scope(scope, snapshot)?;
        let bounds = PageBounds::from_request(request)?;
        if bounds.items == 0 || bounds.bytes == 0 || candidates.is_empty() {
            return Ok(PageStatus::Complete);
        }
        self.validate_snapshot(snapshot).await?;
        let root_project_key = authorized_root_project_key(scope, snapshot)?;
        let control = snapshot.request().execution_control();
        let mut cursor = RecordCursor::decode(request.keyset())?;
        if cursor.candidate >= candidates.len() {
            return Ok(PageStatus::Complete);
        }
        let mut page_bytes = 0usize;
        let window_size = bounds.items.saturating_add(1);
        let mut window_queries = 0usize;
        while cursor.candidate < candidates.len() {
            control.checkpoint()?;
            window_queries += 1;
            if window_queries > bounds.items.saturating_add(1) {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "record candidate window scans",
                });
            }
            let window_end = bounded_window_end(candidates.len(), cursor.candidate, window_size);
            let window = &candidates[cursor.candidate..window_end];
            for candidate in window {
                require_candidate_scope(scope, candidate)?;
                if let Some(project_key) = root_project_key {
                    require_candidate_root_authority(
                        self.read,
                        candidate,
                        project_key,
                        snapshot.provider_scope(),
                    )
                    .await?;
                }
                if candidate.anchor_id.to_string().len() > request.max_key_bytes() {
                    return Err(TemporalPortError::BudgetExceeded {
                        resource: "record candidate anchor bytes",
                    });
                }
            }
            let query_limit = bounds.items.saturating_sub(sink.len()).saturating_add(1);
            let query = build_record_query(
                scope,
                snapshot,
                window,
                cursor.candidate,
                &cursor,
                query_limit,
                request,
            )?;
            let mut rows = self
                .read
                .query(&query.sql, query.params)
                .await
                .map_err(|error| read_error(RECORD_OPERATION, error))?;
            control.checkpoint()?;
            let mut extra = false;
            let mut last_emitted = None;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| read_error(RECORD_OPERATION, error))?
            {
                control.checkpoint()?;
                let row_cursor = RecordCursor::from_row(&row)?;
                if sink.len() == bounds.items {
                    extra = true;
                    break;
                }
                let record = temporal_record_from_row(&row)?;
                let encoded = record.measured_encoded_bytes()?;
                if !fits_bytes(page_bytes, encoded, bounds, request.max_item_bytes()) {
                    if sink.is_empty() {
                        return Err(TemporalPortError::BudgetExceeded {
                            resource: "record bytes",
                        });
                    }
                    extra = true;
                    break;
                }
                page_bytes += encoded;
                last_emitted = Some(row_cursor);
                sink.push(record)?;
            }
            if extra {
                let continuation = last_emitted.unwrap_or(cursor);
                sink.set_continuation_key(continuation.encode(request.max_key_bytes())?)?;
                return Ok(PageStatus::More);
            }
            cursor = RecordCursor {
                candidate: window_end,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            };
            if sink.len() == bounds.items {
                if cursor.candidate < candidates.len() {
                    sink.set_continuation_key(cursor.encode(request.max_key_bytes())?)?;
                    return Ok(PageStatus::More);
                }
                return Ok(PageStatus::Complete);
            }
        }
        Ok(PageStatus::Complete)
    }
}

impl TemporalReadPort for GlobalDbTemporalReadPort<'_> {
    fn produce_candidate_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.produce_candidates(snapshot.retrieval_scope(), snapshot, plan, &request, sink)
                .await
        })
    }

    fn produce_candidate_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        snapshot: &'a TemporalExecutionSnapshot,
        plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.produce_candidates(scope, snapshot, plan, &request, sink)
                .await
        })
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.produce_records(
                snapshot.retrieval_scope(),
                snapshot,
                candidates,
                &request,
                sink,
            )
            .await
        })
    }

    fn produce_temporal_record_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        snapshot: &'a TemporalExecutionSnapshot,
        candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        Box::pin(async move {
            self.produce_records(scope, snapshot, candidates, &request, sink)
                .await
        })
    }
}

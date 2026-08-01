//! SQLite-backed persistence for generation-bound diagnostics (Plan 35,
//! "Universal managed diagnostics"; query/12-diagnostic-persistence authority
//! packet).
//!
//! Durable `GenerationDiagnosticV1` records persist in the project store and
//! survive restarts. Publication is version-monotone: a newer clean
//! generation clears or supersedes prior current records deterministically,
//! stale findings never cross snapshots, and dirty editor overlays live only
//! in memory — they are never sealed into the durable store.

use std::collections::BTreeMap;
use std::fmt;

use tracedecay_domain::{
    CodeGenerationId, DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, FileOccurrenceId, GenerationDiagnosticV1,
    RetrievalAnchorId, SourceSpan, UtcMicros,
};
use tracedecay_store::{
    DIAGNOSTIC_STATE_CLEARED, DIAGNOSTIC_STATE_CURRENT, DIAGNOSTIC_STATE_SUPERSEDED,
    DiagnosticPublicationDispositionV1, DiagnosticPublicationReceiptV1,
    DiagnosticRecordStateKindV1, DiagnosticStore as DiagnosticStorePort, DiagnosticStoreError,
    DiagnosticStoreResult, SanitizedCleanDiagnosticSnapshotV1, diagnostic_evidence_class_name,
    diagnostic_producer_kind_name, diagnostic_severity_name, diagnostic_state_columns,
    parse_diagnostic_evidence_class, parse_diagnostic_producer_kind, parse_diagnostic_severity,
};

use tracedecay_runtime_core::db::MemoryConnection;
use tracedecay_runtime_core::db::engine::{Row, Rows, TransactionBehavior, Value, params};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::tracedecay::current_timestamp;

/// Durable tables for generation-bound diagnostic records.
///
/// The DDL lives in `tracedecay-store` because the rusqlite-runtime
/// `DiagnosticExecutor` writes the same tables and its fixtures must not be
/// weaker than what this engine installs.
pub(crate) const SCHEMA: &str = tracedecay_store::GENERATION_DIAGNOSTICS_SCHEMA_DDL;

// The stored column text is owned by `tracedecay_store::diagnostics::codec` so
// this engine and the rusqlite-runtime `DiagnosticExecutor` cannot drift apart
// across a cutover. These aliases keep the SQL below readable.
const STATE_CURRENT: &str = DIAGNOSTIC_STATE_CURRENT;
const STATE_SUPERSEDED: &str = DIAGNOSTIC_STATE_SUPERSEDED;
const STATE_CLEARED: &str = DIAGNOSTIC_STATE_CLEARED;

/// SQLite-backed store for durable generation-bound diagnostics.
///
/// Records are immutable once persisted; state transitions (supersession,
/// clearing) update only the `record_state`/`state_generation` columns so the
/// full historical chain stays queryable while active publication reads only
/// current rows (Plan 35: "Stale and historical diagnostics remain queryable
/// through `TraceDecay` application APIs but are excluded from active
/// publication").
pub struct DiagnosticsStore<'a> {
    conn: MemoryConnection<'a>,
}

impl<'a> DiagnosticsStore<'a> {
    pub const fn new(conn: &'a tracedecay_runtime_core::db::engine::Connection) -> Self {
        Self {
            conn: MemoryConnection::runtime(conn),
        }
    }

    pub(crate) const fn new_runtime(
        conn: &'a tracedecay_runtime_core::db::engine::Connection,
    ) -> Self {
        Self::new(conn)
    }

    /// Creates the diagnostics schema idempotently. Safe to call on every
    /// open; existing rows are never touched.
    pub async fn ensure_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(SCHEMA)
            .await
            .map_err(|e| db_error("diagnostics ensure_schema", e))
    }

    /// The exact clean generation whose diagnostic snapshot is current.
    ///
    /// Generation publication is recorded even when the snapshot contains no
    /// diagnostics, preserving clean-generation identity across restarts.
    pub async fn current_generation(&self) -> Result<Option<CodeGenerationId>> {
        let operation = "diagnostics current_generation";
        let mut rows = self
            .conn
            .query(
                "SELECT generation_id
                 FROM diagnostic_generation_publications
                 WHERE record_state = ?1",
                params![STATE_CURRENT],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        let generation = rows
            .next()
            .await
            .map_err(|e| db_error(operation, e))?
            .map(|row| {
                row.get::<String>(0)
                    .map_err(|e| db_error(operation, e))
                    .and_then(|value| stored_id(value, operation, "generation_id"))
            })
            .transpose()?;
        if rows
            .next()
            .await
            .map_err(|e| db_error(operation, e))?
            .is_some()
        {
            return Err(db_message(
                operation,
                "multiple current diagnostic generations",
            ));
        }
        Ok(generation)
    }

    /// Runs `work` inside an immediate transaction, committing on success and
    /// rolling back on error or cancellation. The transactional store routes
    /// every statement through that exact transaction.
    async fn with_immediate_tx<T>(
        &self,
        operation: &str,
        work: impl Send
        + for<'tx> FnOnce(
            &'tx DiagnosticsStore<'tx>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T>> + Send + 'tx>,
        >,
    ) -> Result<T>
    where
        T: Send,
    {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|e| db_error(operation, e))?;
        let transactional_store = DiagnosticsStore {
            conn: MemoryConnection::transaction(&transaction),
        };
        match work(&transactional_store).await {
            Ok(value) => {
                transaction
                    .commit()
                    .await
                    .map_err(|error| db_error(operation, error))?;
                Ok(value)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    /// Publishes one clean generation's diagnostic set atomically and
    /// deterministically:
    ///
    /// 1. every supplied record must validate, be in the `Current` state, and
    ///    name exactly `generation` (a clean publication is single-generation
    ///    and carries no stale rows);
    /// 2. every prior `Current` record from any other generation is marked
    ///    `Cleared` by this generation — a clean generation clears prior
    ///    diagnostics, including the empty publication (Plan 35: "A newer
    ///    version clears or supersedes the prior publication
    ///    deterministically");
    /// 3. the new records are inserted idempotently by anchor so republishing
    ///    an unchanged clean generation converges.
    ///
    /// Returns `(inserted, cleared)`.
    pub async fn publish_clean_generation(
        &self,
        generation: &CodeGenerationId,
        records: &[GenerationDiagnosticV1],
    ) -> Result<(u64, u64)> {
        let (inserted, cleared, _) = self
            .publish_clean_generation_with_disposition(generation, records)
            .await?;
        Ok((inserted, cleared))
    }

    async fn publish_clean_generation_with_disposition(
        &self,
        generation: &CodeGenerationId,
        records: &[GenerationDiagnosticV1],
    ) -> Result<(u64, u64, bool)> {
        let operation = "diagnostics publish_clean_generation";
        for record in records {
            record.validate().map_err(|error| {
                db_message(
                    operation,
                    format!(
                        "invalid diagnostic record {}: {error}",
                        record.diagnostic_anchor
                    ),
                )
            })?;
            if record.generation_id != *generation {
                return Err(db_message(
                    operation,
                    format!(
                        "record {} names generation {} but publication targets {}",
                        record.diagnostic_anchor, record.generation_id, generation
                    ),
                ));
            }
            if !record.state.is_current() {
                return Err(db_message(
                    operation,
                    format!(
                        "record {} is not current; stale findings cannot cross snapshots",
                        record.diagnostic_anchor
                    ),
                ));
            }
        }
        let mut records = records.to_vec();
        records.sort_by(|left, right| {
            left.diagnostic_anchor
                .as_str()
                .cmp(right.diagnostic_anchor.as_str())
        });
        if records
            .windows(2)
            .any(|pair| pair[0].diagnostic_anchor == pair[1].diagnostic_anchor)
        {
            return Err(db_message(
                operation,
                "one clean publication cannot contain duplicate diagnostic anchors",
            ));
        }
        let generation = generation.clone();
        self.with_immediate_tx(operation, move |store| Box::pin(async move {
            if let Some(state) = store.generation_publication_state(&generation).await? {
                if state != STATE_CURRENT {
                    return Err(db_message(
                        operation,
                        format!(
                            "generation {generation} is already historical and cannot be republished"
                        ),
                    ));
                }
                let existing = store.current_records(&generation).await?;
                if existing == records {
                    return Ok((0, 0, true));
                }
                return Err(db_message(
                    operation,
                    format!(
                        "generation {generation} already has a different immutable diagnostic snapshot"
                    ),
                ));
            }

            for record in &records {
                if store
                    .record_by_anchor(&record.diagnostic_anchor)
                    .await?
                    .is_some()
                {
                    return Err(db_message(
                        operation,
                        format!(
                            "diagnostic anchor {} is already bound to an immutable record",
                            record.diagnostic_anchor
                        ),
                    ));
                }
            }

            let cleared = store
                .conn
                .execute(
                    "UPDATE generation_diagnostics
                     SET record_state = ?1, state_generation = ?2
                     WHERE record_state = ?3 AND generation_id != ?2",
                    params![STATE_CLEARED, generation.as_str(), STATE_CURRENT],
                )
                .await
                .map_err(|e| db_error(operation, e))?;

            store.conn
                .execute(
                    "UPDATE diagnostic_generation_publications
                     SET record_state = ?1, state_generation = ?2
                     WHERE record_state = ?3 AND generation_id != ?2",
                    params![STATE_CLEARED, generation.as_str(), STATE_CURRENT],
                )
                .await
                .map_err(|e| db_error(operation, e))?;

            for record in &records {
                store.insert_record(record).await?;
            }
            store.conn
                .execute(
                    "INSERT INTO diagnostic_generation_publications (
                        generation_id, record_state, state_generation, published_at
                     ) VALUES (?1, ?2, NULL, ?3)",
                    params![
                        generation.as_str(),
                        STATE_CURRENT,
                        current_timestamp()
                    ],
                )
                .await
                .map_err(|e| db_error(operation, e))?;
            Ok((records.len() as u64, cleared, false))
        }))
        .await
    }

    /// Marks every `Current` record of `prior_generation` as superseded by
    /// `successor_generation`. A generation can never supersede itself.
    /// Returns the number of rows transitioned.
    pub async fn supersede_generation(
        &self,
        prior_generation: &CodeGenerationId,
        successor_generation: &CodeGenerationId,
    ) -> Result<u64> {
        let operation = "diagnostics supersede_generation";
        if prior_generation == successor_generation {
            return Err(db_message(
                operation,
                "a generation cannot supersede itself",
            ));
        }
        let prior_generation = prior_generation.clone();
        let successor_generation = successor_generation.clone();
        self.with_immediate_tx(operation, move |store| {
            Box::pin(async move {
                let transitioned = store
                    .conn
                    .execute(
                        "UPDATE generation_diagnostics
                     SET record_state = ?1, state_generation = ?2
                     WHERE record_state = ?3 AND generation_id = ?4",
                        params![
                            STATE_SUPERSEDED,
                            successor_generation.as_str(),
                            STATE_CURRENT,
                            prior_generation.as_str()
                        ],
                    )
                    .await
                    .map_err(|e| db_error(operation, e))?;
                store
                    .conn
                    .execute(
                        "UPDATE diagnostic_generation_publications
                     SET record_state = ?1, state_generation = ?2
                     WHERE record_state = ?3 AND generation_id = ?4",
                        params![
                            STATE_SUPERSEDED,
                            successor_generation.as_str(),
                            STATE_CURRENT,
                            prior_generation.as_str()
                        ],
                    )
                    .await
                    .map_err(|e| db_error(operation, e))?;
                Ok(transitioned)
            })
        })
        .await
    }

    /// Walks the supersession chain starting at `anchor`. Each step follows
    /// the record's `Superseded { successor_generation }` edge to the current
    /// record in the successor generation with the same logical finding key
    /// (repository, producer, code, file occurrence, span, message digest).
    /// The chain ends at a current, cleared, or missing successor and is
    /// returned oldest-first including the starting record.
    pub async fn supersession_chain(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> Result<Vec<GenerationDiagnosticV1>> {
        let mut chain = Vec::new();
        let Some(start) = self.record_by_anchor(anchor).await? else {
            return Ok(chain);
        };
        chain.push(start);
        loop {
            let Some(last) = chain.last() else {
                // Unreachable: `chain` is seeded before the loop and only
                // grows; bail out with what we have rather than panic.
                return Ok(chain);
            };
            let DiagnosticRecordStateV1::Superseded {
                successor_generation,
            } = &last.state
            else {
                return Ok(chain);
            };
            let successor = self
                .find_logical_successor(last, successor_generation)
                .await?;
            match successor {
                Some(record)
                    if !chain
                        .iter()
                        .any(|seen| seen.diagnostic_anchor == record.diagnostic_anchor) =>
                {
                    chain.push(record);
                }
                _ => return Ok(chain),
            }
        }
    }

    /// All records bound to `generation`, any state, ordered by anchor.
    pub async fn records_for_generation(
        &self,
        generation: &CodeGenerationId,
    ) -> Result<Vec<GenerationDiagnosticV1>> {
        self.query_generation(generation, None).await
    }

    /// Current records bound to `generation` — the only set eligible for
    /// active publication.
    pub async fn current_records(
        &self,
        generation: &CodeGenerationId,
    ) -> Result<Vec<GenerationDiagnosticV1>> {
        self.query_generation(generation, Some(STATE_CURRENT)).await
    }

    /// One bounded page of current records plus the exact lane cardinality.
    pub(crate) async fn current_records_page(
        &self,
        generation: &CodeGenerationId,
        file_occurrence_id: Option<&FileOccurrenceId>,
        after_anchor: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<GenerationDiagnosticV1>, usize, bool)> {
        let operation = "diagnostics current_records_page";
        let fetch_limit = i64::try_from(limit.saturating_add(1)).map_err(|_| {
            db_message(
                operation,
                "diagnostic page limit exceeds the SQLite integer range",
            )
        })?;
        let (sql, query_params) = match (file_occurrence_id, after_anchor) {
            (None, None) => (
                format!(
                    "{SELECT_RECORDS} WHERE generation_id = ?1 AND record_state = ?2 \
                     ORDER BY diagnostic_anchor LIMIT ?3"
                ),
                params![generation.as_str(), STATE_CURRENT, fetch_limit],
            ),
            (None, Some(anchor)) => (
                format!(
                    "{SELECT_RECORDS} WHERE generation_id = ?1 AND record_state = ?2 \
                     AND diagnostic_anchor > ?3 ORDER BY diagnostic_anchor LIMIT ?4"
                ),
                params![generation.as_str(), STATE_CURRENT, anchor, fetch_limit],
            ),
            (Some(file), None) => (
                format!(
                    "{SELECT_RECORDS} WHERE file_occurrence_id = ?1 AND generation_id = ?2 \
                     AND record_state = ?3 ORDER BY diagnostic_anchor LIMIT ?4"
                ),
                params![
                    file.as_str(),
                    generation.as_str(),
                    STATE_CURRENT,
                    fetch_limit
                ],
            ),
            (Some(file), Some(anchor)) => (
                format!(
                    "{SELECT_RECORDS} WHERE file_occurrence_id = ?1 AND generation_id = ?2 \
                     AND record_state = ?3 AND diagnostic_anchor > ?4 \
                     ORDER BY diagnostic_anchor LIMIT ?5"
                ),
                params![
                    file.as_str(),
                    generation.as_str(),
                    STATE_CURRENT,
                    anchor,
                    fetch_limit
                ],
            ),
        };
        let mut rows = self
            .conn
            .query(&sql, query_params)
            .await
            .map_err(|e| db_error(operation, e))?;
        let mut records = collect_rows(&mut rows, operation).await?;
        let has_more = records.len() > limit;
        records.truncate(limit);

        let (count_sql, count_params) = match file_occurrence_id {
            None => (
                "SELECT COUNT(*) FROM generation_diagnostics \
                 WHERE generation_id = ?1 AND record_state = ?2",
                params![generation.as_str(), STATE_CURRENT],
            ),
            Some(file) => (
                "SELECT COUNT(*) FROM generation_diagnostics \
                 WHERE file_occurrence_id = ?1 AND generation_id = ?2 AND record_state = ?3",
                params![file.as_str(), generation.as_str(), STATE_CURRENT],
            ),
        };
        let mut count_rows = self
            .conn
            .query(count_sql, count_params)
            .await
            .map_err(|e| db_error(operation, e))?;
        let total = count_rows
            .next()
            .await
            .map_err(|e| db_error(operation, e))?
            .ok_or_else(|| db_message(operation, "diagnostic count returned no row"))?
            .get::<i64>(0)
            .map_err(|e| db_error(operation, e))?;
        let total = usize::try_from(total)
            .map_err(|_| db_message(operation, "diagnostic count is outside the usize range"))?;
        Ok((records, total, has_more))
    }

    /// Current records for one file occurrence inside `generation`.
    pub async fn current_records_for_file(
        &self,
        generation: &CodeGenerationId,
        file_occurrence_id: &FileOccurrenceId,
    ) -> Result<Vec<GenerationDiagnosticV1>> {
        let operation = "diagnostics current_records_for_file";
        let mut rows = self
            .conn
            .query(
                &format!(
                    "{SELECT_RECORDS} WHERE generation_id = ?1 AND file_occurrence_id = ?2 \
                     AND record_state = ?3 ORDER BY diagnostic_anchor"
                ),
                params![
                    generation.as_str(),
                    file_occurrence_id.as_str(),
                    STATE_CURRENT
                ],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        collect_rows(&mut rows, operation).await
    }

    /// Stale (superseded or cleared) records bound to `generation`. Stale
    /// findings remain queryable but never re-enter active publication.
    pub async fn stale_records(
        &self,
        generation: &CodeGenerationId,
    ) -> Result<Vec<GenerationDiagnosticV1>> {
        let operation = "diagnostics stale_records";
        let mut rows = self
            .conn
            .query(
                &format!(
                    "{SELECT_RECORDS} WHERE generation_id = ?1 AND record_state != ?2 \
                     ORDER BY diagnostic_anchor"
                ),
                params![generation.as_str(), STATE_CURRENT],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        collect_rows(&mut rows, operation).await
    }

    /// Fetches one record by its Plan 13 anchor.
    pub async fn record_by_anchor(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> Result<Option<GenerationDiagnosticV1>> {
        let operation = "diagnostics record_by_anchor";
        let mut rows = self
            .conn
            .query(
                &format!("{SELECT_RECORDS} WHERE diagnostic_anchor = ?1"),
                params![anchor.as_str()],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        let mut records = collect_rows(&mut rows, operation).await?;
        Ok(records.pop())
    }

    /// Merges the durable current set for `generation` with a dirty overlay
    /// for the same generation. The two lanes stay typed and separate: the
    /// overlay lane is session-only data that is never persisted and never
    /// alters the durable lane (Plan 35: "Two clients editing the same file
    /// must not share unsaved analyzer state"; "Dirty-overlay diagnostics are
    /// never sealed into a clean code-intelligence generation").
    pub async fn current_with_overlay(
        &self,
        generation: &CodeGenerationId,
        overlay: &DirtyDiagnosticOverlay,
    ) -> Result<MergedGenerationDiagnostics> {
        if overlay.clean_generation() != generation {
            return Err(db_message(
                "diagnostics current_with_overlay",
                format!(
                    "overlay targets generation {} but query targets {}",
                    overlay.clean_generation(),
                    generation
                ),
            ));
        }
        Ok(MergedGenerationDiagnostics {
            durable: self.current_records(generation).await?,
            overlay_only: overlay.records(),
        })
    }

    async fn insert_record(&self, record: &GenerationDiagnosticV1) -> Result<()> {
        let operation = "diagnostics insert_record";
        let (state, state_generation) = state_columns(&record.state);
        self.conn
            .execute(
                "INSERT INTO generation_diagnostics (
                    diagnostic_anchor, generation_id, repository, worktree, reference,
                    source_revision, file_occurrence_id, content_digest, symbol_occurrence_id,
                    span_start, span_end, code, severity, message, message_digest,
                    producer_kind, producer, analyzer_revision, configuration_revision,
                    sanitization_receipt, evidence_class, collected_at, record_state,
                    state_generation, persisted_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                    ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
                 )",
                params![
                    record.diagnostic_anchor.as_str(),
                    record.generation_id.as_str(),
                    record.repository.as_str(),
                    record
                        .worktree
                        .as_ref()
                        .map(tracedecay_domain::WorktreeId::as_str),
                    record
                        .reference
                        .as_ref()
                        .map(tracedecay_domain::RefId::as_str),
                    record
                        .source_revision
                        .as_ref()
                        .map(tracedecay_domain::CommitId::as_str),
                    record.file_occurrence_id.as_str(),
                    record.content_digest.as_str(),
                    record
                        .symbol_occurrence_id
                        .as_ref()
                        .map(tracedecay_domain::SymbolOccurrenceId::as_str),
                    record.span.start_byte as i64,
                    record.span.end_byte as i64,
                    record.code.as_str(),
                    severity_str(record.severity),
                    record.message.as_str(),
                    record.message_digest.as_str(),
                    producer_kind_str(record.provenance.producer_kind),
                    record.provenance.producer.as_str(),
                    record.provenance.analyzer_revision.as_str(),
                    record.provenance.configuration_revision.as_str(),
                    record
                        .provenance
                        .sanitization_receipt
                        .as_ref()
                        .map(tracedecay_domain::SanitizationReceiptId::as_str),
                    evidence_class_str(record.evidence_class),
                    record.collected_at.0,
                    state,
                    state_generation,
                    current_timestamp(),
                ],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        Ok(())
    }

    async fn generation_publication_state(
        &self,
        generation: &CodeGenerationId,
    ) -> Result<Option<String>> {
        let operation = "diagnostics generation_publication_state";
        let mut rows = self
            .conn
            .query(
                "SELECT record_state
                 FROM diagnostic_generation_publications
                 WHERE generation_id = ?1",
                params![generation.as_str()],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        rows.next()
            .await
            .map_err(|e| db_error(operation, e))?
            .map(|row| row.get::<String>(0).map_err(|e| db_error(operation, e)))
            .transpose()
    }

    async fn query_generation(
        &self,
        generation: &CodeGenerationId,
        state: Option<&str>,
    ) -> Result<Vec<GenerationDiagnosticV1>> {
        let operation = "diagnostics records_for_generation";
        let (sql, params_vec): (String, Vec<Value>) = match state {
            Some(state) => (
                format!(
                    "{SELECT_RECORDS} WHERE generation_id = ?1 AND record_state = ?2 \
                     ORDER BY diagnostic_anchor"
                ),
                vec![
                    Value::Text(generation.as_str().to_owned()),
                    Value::Text(state.to_owned()),
                ],
            ),
            None => (
                format!("{SELECT_RECORDS} WHERE generation_id = ?1 ORDER BY diagnostic_anchor"),
                vec![Value::Text(generation.as_str().to_owned())],
            ),
        };
        let mut rows = self
            .conn
            .query(&sql, params_vec)
            .await
            .map_err(|e| db_error(operation, e))?;
        collect_rows(&mut rows, operation).await
    }

    async fn find_logical_successor(
        &self,
        prior: &GenerationDiagnosticV1,
        successor_generation: &CodeGenerationId,
    ) -> Result<Option<GenerationDiagnosticV1>> {
        let operation = "diagnostics find_logical_successor";
        let mut rows = self
            .conn
            .query(
                &format!(
                    "{SELECT_RECORDS} WHERE generation_id = ?1 AND repository = ?2 \
                     AND producer = ?3 AND code = ?4 AND file_occurrence_id = ?5 \
                     AND span_start = ?6 AND span_end = ?7 AND message_digest = ?8 \
                     ORDER BY diagnostic_anchor"
                ),
                params![
                    successor_generation.as_str(),
                    prior.repository.as_str(),
                    prior.provenance.producer.as_str(),
                    prior.code.as_str(),
                    prior.file_occurrence_id.as_str(),
                    prior.span.start_byte as i64,
                    prior.span.end_byte as i64,
                    prior.message_digest.as_str(),
                ],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        let mut records = collect_rows(&mut rows, operation).await?;
        if records.len() > 1 {
            return Err(db_message(
                operation,
                format!(
                    "ambiguous logical successor for {} in {}",
                    prior.diagnostic_anchor, successor_generation
                ),
            ));
        }
        Ok(records.pop())
    }
}

impl DiagnosticStorePort for DiagnosticsStore<'_> {
    async fn publish_clean_diagnostics(
        &self,
        snapshot: SanitizedCleanDiagnosticSnapshotV1,
    ) -> DiagnosticStoreResult<DiagnosticPublicationReceiptV1> {
        let (generation, records) = snapshot.into_parts();
        let (inserted, cleared, exact_replay) = self
            .publish_clean_generation_with_disposition(&generation, &records)
            .await
            .map_err(|error| port_error("publish_clean_diagnostics", error))?;
        let disposition = if exact_replay {
            DiagnosticPublicationDispositionV1::ExactReplay
        } else {
            DiagnosticPublicationDispositionV1::Committed
        };
        Ok(DiagnosticPublicationReceiptV1::new(
            generation,
            inserted,
            cleared,
            disposition,
        ))
    }

    async fn current_diagnostic_generation(
        &self,
    ) -> DiagnosticStoreResult<Option<CodeGenerationId>> {
        DiagnosticsStore::current_generation(self)
            .await
            .map_err(|error| port_error("current_diagnostic_generation", error))
    }

    async fn diagnostics_for_generation(
        &self,
        generation: &CodeGenerationId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        self.records_for_generation(generation)
            .await
            .map_err(|error| port_error("diagnostics_for_generation", error))
    }

    async fn current_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        self.current_records(generation)
            .await
            .map_err(|error| port_error("current_diagnostics", error))
    }

    async fn current_diagnostics_for_file(
        &self,
        generation: &CodeGenerationId,
        file_occurrence_id: &FileOccurrenceId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        self.current_records_for_file(generation, file_occurrence_id)
            .await
            .map_err(|error| port_error("current_diagnostics_for_file", error))
    }

    async fn stale_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        self.stale_records(generation)
            .await
            .map_err(|error| port_error("stale_diagnostics", error))
    }

    async fn diagnostic_by_anchor(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> DiagnosticStoreResult<Option<GenerationDiagnosticV1>> {
        self.record_by_anchor(anchor)
            .await
            .map_err(|error| port_error("diagnostic_by_anchor", error))
    }

    async fn diagnostic_supersession_chain(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        self.supersession_chain(anchor)
            .await
            .map_err(|error| port_error("diagnostic_supersession_chain", error))
    }

    async fn supersede_diagnostic_generation(
        &self,
        prior_generation: &CodeGenerationId,
        successor_generation: &CodeGenerationId,
    ) -> DiagnosticStoreResult<u64> {
        self.supersede_generation(prior_generation, successor_generation)
            .await
            .map_err(|error| port_error("supersede_diagnostic_generation", error))
    }
}

/// One overlay entry: diagnostics computed against unsaved editor content for
/// one client document version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayDiagnostic {
    pub client_id: String,
    pub document_uri: String,
    pub document_version: i64,
    pub record: GenerationDiagnosticV1,
}

/// In-memory overlay of diagnostics for uncommitted editor buffers.
///
/// The overlay owns no connection and exposes no write path into
/// [`DiagnosticsStore`]: entries live only for the authorized overlay owner's
/// session and are dropped with it (Plan 35: "Unsaved overlays are ephemeral
/// and isolated per client"; overlay findings "are never published as durable
/// LSP diagnostics"). Two clients editing the same file never share entries
/// because every entry is keyed by `(client_id, document_uri)`.
#[derive(Debug)]
pub struct DirtyDiagnosticOverlay {
    clean_generation: CodeGenerationId,
    entries: BTreeMap<(String, String), OverlayDocumentDiagnostics>,
}

#[derive(Debug)]
struct OverlayDocumentDiagnostics {
    document_version: i64,
    records: Vec<OverlayDiagnostic>,
}

impl DirtyDiagnosticOverlay {
    /// Creates an empty overlay bound to the clean generation it overlays.
    pub fn new(clean_generation: CodeGenerationId) -> Self {
        Self {
            clean_generation,
            entries: BTreeMap::new(),
        }
    }

    /// The clean generation this overlay is bound to. Overlays never rebind:
    /// a generation change drops the overlay with its session.
    pub fn clean_generation(&self) -> &CodeGenerationId {
        &self.clean_generation
    }

    /// Replaces one client document's overlay entries for a new document
    /// version. Every record must be current and bound to the overlay's
    /// clean generation; stale findings cannot cross snapshots even in
    /// memory.
    pub fn replace_document(
        &mut self,
        client_id: impl Into<String>,
        document_uri: impl Into<String>,
        document_version: i64,
        records: Vec<GenerationDiagnosticV1>,
    ) -> Result<()> {
        let client_id = client_id.into();
        let document_uri = document_uri.into();
        let key = (client_id.clone(), document_uri.clone());
        if let Some(existing) = self.entries.get(&key)
            && document_version < existing.document_version
        {
            return Err(db_message(
                "diagnostics overlay replace_document",
                format!(
                    "document version {document_version} is older than current version {}",
                    existing.document_version
                ),
            ));
        }
        let clean_generation = self.clean_generation().clone();
        let mut entries = Vec::with_capacity(records.len());
        for record in records {
            record.validate().map_err(|error| {
                db_message(
                    "diagnostics overlay replace_document",
                    format!(
                        "invalid overlay record {}: {error}",
                        record.diagnostic_anchor
                    ),
                )
            })?;
            if record.generation_id != clean_generation {
                return Err(db_message(
                    "diagnostics overlay replace_document",
                    format!(
                        "overlay record {} names generation {} but the overlay is bound to {}",
                        record.diagnostic_anchor, record.generation_id, clean_generation
                    ),
                ));
            }
            if !record.state.is_current() {
                return Err(db_message(
                    "diagnostics overlay replace_document",
                    format!("overlay record {} is not current", record.diagnostic_anchor),
                ));
            }
            entries.push(OverlayDiagnostic {
                client_id: client_id.clone(),
                document_uri: document_uri.clone(),
                document_version,
                record,
            });
        }
        self.entries.insert(
            key,
            OverlayDocumentDiagnostics {
                document_version,
                records: entries,
            },
        );
        Ok(())
    }

    /// Drops one client document's entries (editor closed or overlay
    /// released by session TTL).
    pub fn remove_document(&mut self, client_id: &str, document_uri: &str) {
        self.entries
            .remove(&(client_id.to_owned(), document_uri.to_owned()));
    }

    /// Releases every overlay entry, as on session expiry.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.entries
            .values()
            .all(|document| document.records.is_empty())
    }

    pub fn len(&self) -> usize {
        self.entries
            .values()
            .map(|document| document.records.len())
            .sum()
    }

    /// Overlay entries for one client document, oldest insertion first.
    pub fn records_for(&self, client_id: &str, document_uri: &str) -> &[OverlayDiagnostic] {
        self.entries
            .get(&(client_id.to_owned(), document_uri.to_owned()))
            .map_or(&[], |document| document.records.as_slice())
    }

    /// Every overlay entry across all client documents, in deterministic key
    /// order. Session-only; never persisted.
    pub fn records(&self) -> Vec<OverlayDiagnostic> {
        self.entries
            .values()
            .flat_map(|document| document.records.iter().cloned())
            .collect()
    }
}

/// The merged read model for one generation: durable current records and
/// session-only overlay records in separate typed lanes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergedGenerationDiagnostics {
    pub durable: Vec<GenerationDiagnosticV1>,
    pub overlay_only: Vec<OverlayDiagnostic>,
}

const SELECT_RECORDS: &str = "SELECT diagnostic_anchor, generation_id, repository, worktree,
        reference, source_revision, file_occurrence_id, content_digest, symbol_occurrence_id,
        span_start, span_end, code, severity, message, message_digest, producer_kind, producer,
        analyzer_revision, configuration_revision, sanitization_receipt, evidence_class,
        collected_at, record_state, state_generation
     FROM generation_diagnostics";

async fn collect_rows(rows: &mut Rows, operation: &str) -> Result<Vec<GenerationDiagnosticV1>> {
    let mut records = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| db_error(operation, e))? {
        records.push(record_from_row(&row, operation)?);
    }
    Ok(records)
}

fn record_from_row(row: &Row, operation: &str) -> Result<GenerationDiagnosticV1> {
    let text = |index: i32| row.get::<String>(index).map_err(|e| db_error(operation, e));
    let optional_text = |index: i32| {
        row.get::<Option<String>>(index)
            .map_err(|e| db_error(operation, e))
    };

    let stored_state = text(22)?;
    let kind = DiagnosticRecordStateKindV1::parse(&stored_state).ok_or_else(|| {
        db_message(
            operation,
            format!("unknown diagnostic record state: {stored_state}"),
        )
    })?;
    let state_generation = match kind.state_generation_field() {
        Some(field) => Some(stored_id(
            optional_text(23)?.ok_or_else(|| {
                db_message(
                    operation,
                    match kind {
                        DiagnosticRecordStateKindV1::Cleared => {
                            "cleared record missing state_generation"
                        }
                        _ => "superseded record missing state_generation",
                    },
                )
            })?,
            operation,
            field,
        )?),
        None => None,
    };
    let state = kind
        .into_state(state_generation)
        .ok_or_else(|| db_message(operation, "current record carries a state_generation"))?;

    Ok(GenerationDiagnosticV1 {
        diagnostic_anchor: stored_id(text(0)?, operation, "diagnostic_anchor")?,
        generation_id: stored_id(text(1)?, operation, "generation_id")?,
        repository: stored_id(text(2)?, operation, "repository")?,
        worktree: optional_text(3)?
            .map(|value| stored_id(value, operation, "worktree"))
            .transpose()?,
        reference: optional_text(4)?
            .map(|value| stored_id(value, operation, "reference"))
            .transpose()?,
        source_revision: optional_text(5)?
            .map(|value| stored_id(value, operation, "source_revision"))
            .transpose()?,
        file_occurrence_id: stored_id(text(6)?, operation, "file_occurrence_id")?,
        content_digest: stored_id(text(7)?, operation, "content_digest")?,
        symbol_occurrence_id: optional_text(8)?
            .map(|value| stored_id(value, operation, "symbol_occurrence_id"))
            .transpose()?,
        span: SourceSpan {
            start_byte: row.get::<i64>(9).map_err(|e| db_error(operation, e))? as u64,
            end_byte: row.get::<i64>(10).map_err(|e| db_error(operation, e))? as u64,
        },
        code: text(11)?,
        severity: parse_severity(&text(12)?, operation)?,
        message: text(13)?,
        message_digest: stored_id(text(14)?, operation, "message_digest")?,
        provenance: DiagnosticProvenanceV1 {
            producer_kind: parse_producer_kind(&text(15)?, operation)?,
            producer: stored_id(text(16)?, operation, "producer")?,
            analyzer_revision: stored_id(text(17)?, operation, "analyzer_revision")?,
            configuration_revision: stored_id(text(18)?, operation, "configuration_revision")?,
            sanitization_receipt: optional_text(19)?
                .map(|value| stored_id(value, operation, "sanitization_receipt"))
                .transpose()?,
        },
        evidence_class: parse_evidence_class(&text(20)?, operation)?,
        collected_at: UtcMicros(row.get::<i64>(21).map_err(|e| db_error(operation, e))?),
        state,
    })
}

/// Rebuilds a strongly typed domain identity from its stored text form.
fn stored_id<T>(value: String, operation: &str, field: &'static str) -> Result<T>
where
    T: TryFrom<String, Error = tracedecay_domain::DomainError>,
{
    T::try_from(value).map_err(|error| db_message(operation, format!("{field}: {error}")))
}

// The mappings below delegate to the shared store codec; only the failure
// wording stays local, because it is observable in this engine's errors.

fn state_columns(state: &DiagnosticRecordStateV1) -> (&'static str, Option<String>) {
    let (column, state_generation) = diagnostic_state_columns(state);
    (column, state_generation.map(str::to_owned))
}

fn severity_str(severity: DiagnosticSeverityV1) -> &'static str {
    diagnostic_severity_name(severity)
}

fn parse_severity(value: &str, operation: &str) -> Result<DiagnosticSeverityV1> {
    parse_diagnostic_severity(value).ok_or_else(|| {
        db_message(
            operation,
            format!("failed to parse diagnostic severity: {value}"),
        )
    })
}

fn producer_kind_str(kind: DiagnosticProducerKindV1) -> &'static str {
    diagnostic_producer_kind_name(kind)
}

fn parse_producer_kind(value: &str, operation: &str) -> Result<DiagnosticProducerKindV1> {
    parse_diagnostic_producer_kind(value).ok_or_else(|| {
        db_message(
            operation,
            format!("failed to parse diagnostic producer kind: {value}"),
        )
    })
}

fn evidence_class_str(class: DiagnosticEvidenceClassV1) -> &'static str {
    diagnostic_evidence_class_name(class)
}

fn parse_evidence_class(value: &str, operation: &str) -> Result<DiagnosticEvidenceClassV1> {
    parse_diagnostic_evidence_class(value).ok_or_else(|| {
        db_message(
            operation,
            format!("failed to parse diagnostic evidence class: {value}"),
        )
    })
}

fn port_error(operation: &'static str, source: TraceDecayError) -> DiagnosticStoreError {
    DiagnosticStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

fn db_error(operation: &str, error: impl fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: error.to_string(),
        operation: operation.to_string(),
    }
}

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn fixture_record(generation: &str, anchor: &str) -> GenerationDiagnosticV1 {
        let mut record = GenerationDiagnosticV1 {
            diagnostic_anchor: id(anchor),
            generation_id: id(generation),
            repository: id("repository.fixture"),
            worktree: Some(id("worktree.fixture")),
            reference: Some(id("ref.main")),
            source_revision: Some(id("commit.abc123")),
            file_occurrence_id: id("file.occurrence.1"),
            content_digest: id(&digest('a')),
            span: SourceSpan {
                start_byte: 10,
                end_byte: 42,
            },
            symbol_occurrence_id: Some(id("symbol.occurrence.1")),
            code: "E0308".to_owned(),
            severity: DiagnosticSeverityV1::Error,
            message: "mismatched types".to_owned(),
            message_digest: id(&digest('b')),
            provenance: DiagnosticProvenanceV1 {
                producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
                producer: id("producer.rustc"),
                analyzer_revision: id("analyzer.v1"),
                configuration_revision: id("config.v1"),
                sanitization_receipt: Some(id("receipt.sanitization.1")),
            },
            evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
            collected_at: UtcMicros(1_700_000_000_000_000),
            state: DiagnosticRecordStateV1::Current,
        };
        record.message_digest = record
            .compute_message_digest()
            .expect("canonical message digest");
        record
    }

    fn with_message(
        base: GenerationDiagnosticV1,
        code: &str,
        message: &str,
    ) -> GenerationDiagnosticV1 {
        let mut record = GenerationDiagnosticV1 {
            code: code.to_owned(),
            message: message.to_owned(),
            ..base
        };
        record.message_digest = record
            .compute_message_digest()
            .expect("canonical message digest");
        record
    }

    async fn open_store(
        path: &std::path::Path,
    ) -> tracedecay_runtime_core::db::engine::TestConnection {
        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(path);
        DiagnosticsStore::new_runtime(&conn)
            .ensure_schema()
            .await
            .expect("ensure diagnostics schema");
        conn
    }

    #[tokio::test]
    async fn persist_restart_read_back() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let gen1 = "generation.clean.1";
        let first = fixture_record(gen1, "anchor.diagnostic.1");
        let second = GenerationDiagnosticV1 {
            severity: DiagnosticSeverityV1::Warning,
            ..with_message(
                fixture_record(gen1, "anchor.diagnostic.2"),
                "dead_code",
                "function is never used",
            )
        };

        {
            let conn = open_store(&path).await;
            let store = DiagnosticsStore::new_runtime(&conn);
            let (inserted, cleared) = store
                .publish_clean_generation(&id(gen1), &[first.clone(), second.clone()])
                .await
                .expect("publish generation");
            assert_eq!((inserted, cleared), (2, 0));
        }

        // Simulate a restart: new database handle and connection on the same
        // file, with the schema ensured again idempotently.
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let records = store
            .records_for_generation(&id(gen1))
            .await
            .expect("read back after restart");
        assert_eq!(records, vec![first.clone(), second.clone()]);
        assert_eq!(
            store
                .record_by_anchor(&id("anchor.diagnostic.1"))
                .await
                .expect("anchor read"),
            Some(first)
        );
    }

    #[tokio::test]
    async fn clean_generation_clears_prior_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let gen1 = "generation.clean.1";
        let gen2 = "generation.clean.2";

        store
            .publish_clean_generation(
                &id(gen1),
                &[
                    fixture_record(gen1, "anchor.diagnostic.1"),
                    fixture_record(gen1, "anchor.diagnostic.2"),
                ],
            )
            .await
            .unwrap();

        // A clean generation with no findings deterministically clears the
        // prior publication.
        let (inserted, cleared) = store
            .publish_clean_generation(&id(gen2), &[])
            .await
            .expect("empty clean publication");
        assert_eq!((inserted, cleared), (0, 2));

        assert!(store.current_records(&id(gen1)).await.unwrap().is_empty());
        let stale = store.stale_records(&id(gen1)).await.unwrap();
        assert_eq!(stale.len(), 2);
        assert!(stale.iter().all(|record| matches!(
            &record.state,
            DiagnosticRecordStateV1::Cleared {
                cleared_in_generation
            } if cleared_in_generation.as_str() == gen2
        )));
        // History stays queryable after clearing.
        assert_eq!(
            store.records_for_generation(&id(gen1)).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn supersession_marks_old_records_and_chains() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let gen1 = "generation.clean.1";
        let gen2 = "generation.clean.2";

        let prior = fixture_record(gen1, "anchor.diagnostic.1");
        store
            .publish_clean_generation(&id(gen1), std::slice::from_ref(&prior))
            .await
            .unwrap();

        assert!(
            store
                .supersede_generation(&id(gen1), &id(gen1))
                .await
                .is_err(),
            "a generation cannot supersede itself"
        );

        assert_eq!(
            store
                .supersede_generation(&id(gen1), &id(gen2))
                .await
                .unwrap(),
            1
        );
        let marked = store
            .record_by_anchor(&id("anchor.diagnostic.1"))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            &marked.state,
            DiagnosticRecordStateV1::Superseded {
                successor_generation
            } if successor_generation.as_str() == gen2
        ));
        assert!(!marked.is_current());

        // The successor publication republishes the same logical finding
        // under a new anchor; the chain walks old -> new.
        let successor = fixture_record(gen2, "anchor.diagnostic.2");
        store
            .publish_clean_generation(&id(gen2), std::slice::from_ref(&successor))
            .await
            .unwrap();
        let chain = store
            .supersession_chain(&id("anchor.diagnostic.1"))
            .await
            .unwrap();
        assert_eq!(chain, vec![prior.supersede(id(gen2)).unwrap(), successor]);
    }

    #[tokio::test]
    async fn dirty_overlay_never_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let gen1 = "generation.clean.1";

        let durable = fixture_record(gen1, "anchor.diagnostic.1");
        store
            .publish_clean_generation(&id(gen1), std::slice::from_ref(&durable))
            .await
            .unwrap();

        let mut overlay = DirtyDiagnosticOverlay::new(id(gen1));
        let overlay_record = with_message(
            fixture_record(gen1, "anchor.overlay.1"),
            "unused_variables",
            "unused variable: `tmp`",
        );
        overlay
            .replace_document(
                "client.a",
                "file:///src/main.rs",
                7,
                vec![overlay_record.clone()],
            )
            .expect("overlay accepts current clean-generation record");

        // Overlays are generation-bound: stale or foreign-generation records
        // are rejected even in memory.
        assert!(
            overlay
                .replace_document(
                    "client.a",
                    "file:///src/lib.rs",
                    1,
                    vec![fixture_record("generation.clean.2", "anchor.overlay.2")],
                )
                .is_err()
        );

        // The merged read keeps durable and overlay lanes separate.
        let merged = store
            .current_with_overlay(&id(gen1), &overlay)
            .await
            .unwrap();
        assert_eq!(merged.durable, vec![durable]);
        assert_eq!(merged.overlay_only.len(), 1);
        assert_eq!(merged.overlay_only[0].record, overlay_record);
        assert_eq!(merged.overlay_only[0].document_version, 7);

        // Dropping the overlay leaves the durable store untouched: only the
        // clean generation's row was ever persisted.
        drop(overlay);
        assert_eq!(
            store.records_for_generation(&id(gen1)).await.unwrap().len(),
            1
        );
        assert!(
            store
                .record_by_anchor(&id("anchor.overlay.1"))
                .await
                .unwrap()
                .is_none(),
            "overlay records are never persisted"
        );
    }

    #[tokio::test]
    async fn queries_filter_by_generation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let gen1 = "generation.clean.1";
        let gen2 = "generation.clean.2";

        store
            .publish_clean_generation(
                &id(gen1),
                &[
                    fixture_record(gen1, "anchor.diagnostic.1"),
                    fixture_record(gen1, "anchor.diagnostic.2"),
                ],
            )
            .await
            .unwrap();
        store
            .publish_clean_generation(
                &id(gen2),
                std::slice::from_ref(&fixture_record(gen2, "anchor.diagnostic.3")),
            )
            .await
            .unwrap();

        assert_eq!(
            store.records_for_generation(&id(gen1)).await.unwrap().len(),
            2
        );
        let gen2_records = store.records_for_generation(&id(gen2)).await.unwrap();
        assert_eq!(gen2_records.len(), 1);
        assert!(
            gen2_records
                .iter()
                .all(|record| record.generation_id.as_str() == gen2)
        );
        assert_eq!(
            store
                .current_records_for_file(&id(gen2), &id("file.occurrence.1"))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .current_records_for_file(&id(gen1), &id("file.occurrence.1"))
                .await
                .unwrap()
                .is_empty(),
            "gen1 rows were cleared by the gen2 clean publication"
        );
    }

    #[tokio::test]
    async fn publication_rejects_stale_or_mixed_generation_records() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let gen1 = "generation.clean.1";
        let gen2 = "generation.clean.2";

        // Stale findings cannot cross snapshots: publishing a non-current
        // record is rejected.
        let stale = fixture_record(gen1, "anchor.diagnostic.1")
            .supersede(id(gen2))
            .unwrap();
        assert!(
            store
                .publish_clean_generation(&id(gen1), std::slice::from_ref(&stale))
                .await
                .is_err()
        );

        // A clean publication is single-generation.
        assert!(
            store
                .publish_clean_generation(
                    &id(gen1),
                    &[
                        fixture_record(gen1, "anchor.diagnostic.1"),
                        fixture_record(gen2, "anchor.diagnostic.2"),
                    ],
                )
                .await
                .is_err()
        );

        // Overlay release drops every entry without touching the store.
        let mut overlay = DirtyDiagnosticOverlay::new(id(gen1));
        overlay
            .replace_document(
                "client.a",
                "file:///src/main.rs",
                1,
                vec![fixture_record(gen1, "anchor.overlay.1")],
            )
            .unwrap();
        assert_eq!(overlay.len(), 1);
        overlay.clear();
        assert!(overlay.is_empty());
        assert!(
            store
                .records_for_generation(&id(gen1))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn empty_clean_generation_identity_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let gen1 = id("generation.clean.empty");

        {
            let conn = open_store(&path).await;
            let store = DiagnosticsStore::new_runtime(&conn);
            assert_eq!(
                store.publish_clean_generation(&gen1, &[]).await.unwrap(),
                (0, 0)
            );
        }

        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        assert_eq!(
            store.current_generation().await.unwrap(),
            Some(gen1.clone())
        );
        assert_eq!(
            store.publish_clean_generation(&gen1, &[]).await.unwrap(),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn unchanged_clean_generation_republication_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let gen1 = "generation.clean.1";
        let record = fixture_record(gen1, "anchor.diagnostic.1");

        assert_eq!(
            store
                .publish_clean_generation(&id(gen1), std::slice::from_ref(&record))
                .await
                .unwrap(),
            (1, 0)
        );
        assert_eq!(
            store
                .publish_clean_generation(&id(gen1), std::slice::from_ref(&record))
                .await
                .unwrap(),
            (0, 0)
        );
        assert_eq!(
            store.records_for_generation(&id(gen1)).await.unwrap(),
            vec![record]
        );
    }

    #[tokio::test]
    async fn immutable_anchor_collision_rolls_back_current_generation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let gen1 = "generation.clean.1";
        let gen2 = "generation.clean.2";
        let original = fixture_record(gen1, "anchor.diagnostic.shared");

        store
            .publish_clean_generation(&id(gen1), std::slice::from_ref(&original))
            .await
            .unwrap();

        let collision = fixture_record(gen2, "anchor.diagnostic.shared");
        assert!(
            store
                .publish_clean_generation(&id(gen2), &[collision])
                .await
                .is_err(),
            "one durable anchor must never be rebound to another generation"
        );
        assert_eq!(
            store.current_records(&id(gen1)).await.unwrap(),
            vec![original]
        );
        assert!(
            store
                .records_for_generation(&id(gen2))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn cleared_or_superseded_generation_cannot_be_reactivated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let gen1 = "generation.clean.1";
        let gen2 = "generation.clean.2";
        let gen3 = "generation.clean.3";
        let cleared = fixture_record(gen1, "anchor.diagnostic.cleared");

        store
            .publish_clean_generation(&id(gen1), std::slice::from_ref(&cleared))
            .await
            .unwrap();
        store
            .publish_clean_generation(&id(gen2), &[])
            .await
            .unwrap();
        assert!(
            store
                .publish_clean_generation(&id(gen1), std::slice::from_ref(&cleared))
                .await
                .is_err(),
            "a cleared generation must stay historical"
        );

        let superseded = fixture_record(gen3, "anchor.diagnostic.superseded");
        store
            .publish_clean_generation(&id(gen3), std::slice::from_ref(&superseded))
            .await
            .unwrap();
        store
            .supersede_generation(&id(gen3), &id("generation.clean.4"))
            .await
            .unwrap();
        assert!(
            store
                .publish_clean_generation(&id(gen3), std::slice::from_ref(&superseded))
                .await
                .is_err(),
            "a superseded generation must stay historical"
        );
    }

    #[test]
    fn dirty_overlay_rejects_older_document_versions() {
        let gen1 = "generation.clean.1";
        let mut overlay = DirtyDiagnosticOverlay::new(id(gen1));
        overlay
            .replace_document(
                "client.a",
                "file:///src/main.rs",
                7,
                vec![fixture_record(gen1, "anchor.overlay.current")],
            )
            .unwrap();

        assert!(
            overlay
                .replace_document(
                    "client.a",
                    "file:///src/main.rs",
                    6,
                    vec![fixture_record(gen1, "anchor.overlay.stale")],
                )
                .is_err()
        );
        let records = overlay.records_for("client.a", "file:///src/main.rs");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].document_version, 7);

        overlay
            .replace_document("client.a", "file:///src/main.rs", 8, Vec::new())
            .unwrap();
        assert!(
            overlay
                .records_for("client.a", "file:///src/main.rs")
                .is_empty()
        );
        assert!(
            overlay
                .replace_document(
                    "client.a",
                    "file:///src/main.rs",
                    7,
                    vec![fixture_record(gen1, "anchor.overlay.stale.after-clear")],
                )
                .is_err(),
            "an empty newer overlay snapshot must still fence stale updates"
        );
    }

    #[test]
    fn root_store_implements_diagnostic_persistence_port() {
        fn assert_store<T: tracedecay_store::DiagnosticStore>() {}
        assert_store::<DiagnosticsStore<'static>>();
    }

    #[tokio::test]
    async fn root_port_reports_commit_and_exact_replay() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("diagnostics.db");
        let conn = open_store(&path).await;
        let store = DiagnosticsStore::new_runtime(&conn);
        let generation = id("generation.clean.1");
        let snapshot = SanitizedCleanDiagnosticSnapshotV1::new(
            generation,
            vec![fixture_record("generation.clean.1", "anchor.diagnostic.1")],
        )
        .unwrap();

        let committed = store
            .publish_clean_diagnostics(snapshot.clone())
            .await
            .unwrap();
        assert_eq!(
            committed.disposition(),
            DiagnosticPublicationDispositionV1::Committed
        );
        assert_eq!(committed.inserted_records(), 1);

        let replayed = store.publish_clean_diagnostics(snapshot).await.unwrap();
        assert_eq!(
            replayed.disposition(),
            DiagnosticPublicationDispositionV1::ExactReplay
        );
        assert_eq!(replayed.inserted_records(), 0);
        assert_eq!(replayed.cleared_records(), 0);
    }
}

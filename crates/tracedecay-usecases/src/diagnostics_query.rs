//! Typed diagnostic query core over [`DiagnosticsStore`] (Plan 35, "Universal
//! managed diagnostics"; query/22-diagnostic-query-core packet).
//!
//! This module is a pure read path: no writes, no repair, no schema changes.
//! Every lane returns domain records with explicit coverage — `Complete`,
//! `Truncated`, or `StoreUnavailable` — so a partial or failed read is never
//! presented as a clean result (Plan 35: "Partial coverage is never
//! represented as a clean result"). All list lanes are bounded by a limit
//! plus an opaque cursor and are deterministic: records page in ascending
//! anchor order, chains page in chain order.
//!
//! Supersession navigation mirrors the store's logical finding key exactly
//! (repository, producer, code, file occurrence, span, message digest):
//! forward walks follow `Superseded { successor_generation }` edges toward
//! newer records, backward walks invert those edges toward older records.
//!
//! The overlay merge composes a session-only [`DirtyDiagnosticOverlay`] with
//! the durable current set into one deterministic view; the overlay wins on
//! the same logical finding key and every entry is marked with its
//! provenance (persisted vs overlay). Overlay state is never persisted.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use tracedecay_domain::{
    CodeGenerationId, DiagnosticRecordStateV1, FileOccurrenceId, GenerationDiagnosticV1,
    RetrievalAnchorId,
};

use crate::diagnostics_store::{DiagnosticsStore, DirtyDiagnosticOverlay};
use tracedecay_runtime_core::db::engine::{Connection, params};
use tracedecay_runtime_core::errors::{Result as CrateResult, TraceDecayError};

/// Default page limit when a request carries `limit: 0`.
pub const DEFAULT_DIAGNOSTIC_PAGE_LIMIT: usize = 200;

/// Hard cap on any single page, regardless of the requested limit.
pub const MAX_DIAGNOSTIC_PAGE_LIMIT: usize = 1000;

const CURSOR_PREFIX: &str = "dq1:";

/// Explicit coverage for every diagnostic query lane. A read is either
/// complete, deterministically truncated with a resumption cursor, or
/// unavailable because the store could not answer — never silently partial
/// (Plan 35: engine status and dropped updates "remain visible through typed
/// status").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticQueryCoverage {
    /// The lane returned every matching record.
    Complete,
    /// The lane hit the page limit; resume with the page's `next_cursor`.
    Truncated,
    /// The store could not answer (for example a closed connection). The
    /// record set is empty and the reason is carried, never swallowed.
    StoreUnavailable {
        operation: &'static str,
        reason: String,
    },
}

/// Opaque resumption cursor for bounded diagnostic queries. Cursors encode
/// the anchor of the last record of the previous page; they are valid only
/// for the lane that minted them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticQueryCursor(String);

impl DiagnosticQueryCursor {
    pub(crate) fn after_anchor(anchor: &RetrievalAnchorId) -> Self {
        Self(format!("{CURSOR_PREFIX}{}", anchor.as_str()))
    }

    /// The opaque wire form of this cursor.
    pub fn encode(&self) -> &str {
        &self.0
    }

    /// Rebuilds a cursor from its opaque wire form.
    pub fn decode(encoded: &str) -> Result<Self, DiagnosticQueryError> {
        if !encoded.starts_with(CURSOR_PREFIX) || encoded.len() == CURSOR_PREFIX.len() {
            return Err(DiagnosticQueryError::InvalidCursor {
                cursor: encoded.to_owned(),
            });
        }
        Ok(Self(encoded.to_owned()))
    }

    pub(crate) fn anchor(&self) -> &str {
        &self.0[CURSOR_PREFIX.len()..]
    }
}

/// Bounded page request: a limit (0 selects the default, anything above the
/// hard cap is clamped) plus an optional resumption cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticPageRequest {
    pub limit: usize,
    pub cursor: Option<DiagnosticQueryCursor>,
}

impl DiagnosticPageRequest {
    pub fn new(limit: usize, cursor: Option<DiagnosticQueryCursor>) -> Self {
        Self { limit, cursor }
    }
}

/// One bounded page of diagnostic records with explicit coverage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticPage {
    pub records: Vec<GenerationDiagnosticV1>,
    pub total: usize,
    pub coverage: DiagnosticQueryCoverage,
    pub next_cursor: Option<DiagnosticQueryCursor>,
}

impl DiagnosticPage {
    fn unavailable(operation: &'static str, error: impl fmt::Display) -> Self {
        Self {
            records: Vec::new(),
            total: 0,
            coverage: DiagnosticQueryCoverage::StoreUnavailable {
                operation,
                reason: error.to_string(),
            },
            next_cursor: None,
        }
    }
}

/// Point lookup of one record by its Plan 13 anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticAnchorLookup {
    pub record: Option<GenerationDiagnosticV1>,
    pub coverage: DiagnosticQueryCoverage,
}

/// Exact clean generation currently eligible for active diagnostic reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentDiagnosticGeneration {
    pub generation: Option<CodeGenerationId>,
    pub coverage: DiagnosticQueryCoverage,
}

/// One persisted finding republished by a successor generation under a new
/// anchor: the prior record and its successor share one logical finding key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticSupersessionPair {
    pub prior: GenerationDiagnosticV1,
    pub successor: GenerationDiagnosticV1,
}

/// Generation-aware answer to "what changed for this file between
/// generations `from_generation` and `to_generation`", computed from the
/// store's records and supersession chains. Lanes are deterministic
/// (ascending anchor order; pairs ordered by successor anchor) and each lane
/// is capped at the request limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationDiagnosticDiff {
    pub from_generation: CodeGenerationId,
    pub to_generation: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    /// Findings present in `to_generation` with no same-key record in
    /// `from_generation`.
    pub introduced: Vec<GenerationDiagnosticV1>,
    /// Findings present in both generations: the `from_generation` record
    /// was superseded (or otherwise carried) into the `to_generation` record
    /// for the same logical finding key.
    pub superseded: Vec<DiagnosticSupersessionPair>,
    /// Findings present in `from_generation` with no same-key record in
    /// `to_generation`.
    pub cleared: Vec<GenerationDiagnosticV1>,
    pub coverage: DiagnosticQueryCoverage,
}

/// Where one entry of the merged current view came from. The durable lane
/// and the session-only overlay lane stay typed and separate even after
/// merging (Plan 35: overlay findings "are never published as durable LSP
/// diagnostics").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergedDiagnosticProvenance {
    /// A durable record persisted in the store for the clean generation.
    Persisted,
    /// A session-only overlay entry for one client document version.
    Overlay {
        client_id: String,
        document_uri: String,
        document_version: i64,
    },
}

/// One entry of the merged current view: a domain record plus its lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedDiagnostic {
    pub record: GenerationDiagnosticV1,
    pub provenance: MergedDiagnosticProvenance,
}

/// The deterministic merged view of durable current records and overlay
/// records for one generation. On the same logical finding key the overlay
/// entry wins; ordering is ascending anchor of the winning record with
/// provenance as tie-break.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedDiagnosticView {
    pub entries: Vec<MergedDiagnostic>,
    pub coverage: DiagnosticQueryCoverage,
    pub next_cursor: Option<DiagnosticQueryCursor>,
}

impl MergedDiagnosticView {
    fn unavailable(operation: &'static str, error: impl fmt::Display) -> Self {
        Self {
            entries: Vec::new(),
            coverage: DiagnosticQueryCoverage::StoreUnavailable {
                operation,
                reason: error.to_string(),
            },
            next_cursor: None,
        }
    }
}

/// Caller-side query errors. Store failures are never reported through this
/// type — they surface as [`DiagnosticQueryCoverage::StoreUnavailable`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticQueryError {
    /// The cursor is malformed or does not name a record on the chain lane
    /// it was minted from.
    InvalidCursor { cursor: String },
    /// A generation diff requires two distinct generations.
    SameGeneration { generation: CodeGenerationId },
    /// The overlay is bound to a different clean generation than the query.
    OverlayGenerationMismatch {
        overlay_generation: CodeGenerationId,
        query_generation: CodeGenerationId,
    },
}

impl fmt::Display for DiagnosticQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCursor { cursor } => {
                write!(formatter, "invalid diagnostic query cursor: {cursor}")
            }
            Self::SameGeneration { generation } => write!(
                formatter,
                "a generation diagnostic diff requires two distinct generations, got {generation} twice"
            ),
            Self::OverlayGenerationMismatch {
                overlay_generation,
                query_generation,
            } => write!(
                formatter,
                "overlay targets generation {overlay_generation} but the query targets {query_generation}"
            ),
        }
    }
}

impl std::error::Error for DiagnosticQueryError {}

/// The logical finding key, mirroring the store's supersession successor
/// match exactly: (repository, producer, code, file occurrence, span,
/// message digest). Records sharing a key are the same logical finding
/// republished across generations.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LogicalFindingKey {
    repository: String,
    producer: String,
    code: String,
    file_occurrence_id: String,
    span_start: u64,
    span_end: u64,
    message_digest: String,
}

impl LogicalFindingKey {
    fn of(record: &GenerationDiagnosticV1) -> Self {
        Self {
            repository: record.repository.as_str().to_owned(),
            producer: record.provenance.producer.as_str().to_owned(),
            code: record.code.clone(),
            file_occurrence_id: record.file_occurrence_id.as_str().to_owned(),
            span_start: record.span.start_byte,
            span_end: record.span.end_byte,
            message_digest: record.message_digest.as_str().to_owned(),
        }
    }
}

/// Typed, bounded, read-only query surface over [`DiagnosticsStore`].
///
/// Owns no connection of its own and exposes no write path; every method
/// borrows the store's connection for the duration of one read.
pub struct DiagnosticsQuery<'a> {
    conn: &'a Connection,
    store: DiagnosticsStore<'a>,
}

impl<'a> DiagnosticsQuery<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self {
            conn,
            store: DiagnosticsStore::new_runtime(conn),
        }
    }

    /// Reads the clean-generation publication pointer. A completed empty
    /// publication returns `Some(generation)` even when it contains no
    /// findings; no pointer is distinct from a clean result.
    pub async fn current_generation(&self) -> CurrentDiagnosticGeneration {
        let operation = "diagnostics query current_generation";
        match self.store.current_generation().await {
            Ok(generation) => CurrentDiagnosticGeneration {
                generation,
                coverage: DiagnosticQueryCoverage::Complete,
            },
            Err(error) => CurrentDiagnosticGeneration {
                generation: None,
                coverage: DiagnosticQueryCoverage::StoreUnavailable {
                    operation,
                    reason: error.to_string(),
                },
            },
        }
    }

    /// Current records bound to `generation`, paged in ascending anchor
    /// order.
    pub async fn current_by_generation(
        &self,
        generation: &CodeGenerationId,
        request: &DiagnosticPageRequest,
    ) -> Result<DiagnosticPage, DiagnosticQueryError> {
        let operation = "diagnostics query current_by_generation";
        match self
            .store
            .current_records_page(
                generation,
                None,
                request.cursor.as_ref().map(DiagnosticQueryCursor::anchor),
                normalize_limit(request.limit),
            )
            .await
        {
            Ok((records, total, has_more)) => {
                Ok(page_from_bounded_records(records, total, has_more))
            }
            Err(error) => Ok(DiagnosticPage::unavailable(operation, error)),
        }
    }

    /// Current records for one file occurrence inside `generation`, paged in
    /// ascending anchor order.
    pub async fn current_by_file(
        &self,
        generation: &CodeGenerationId,
        file_occurrence_id: &FileOccurrenceId,
        request: &DiagnosticPageRequest,
    ) -> Result<DiagnosticPage, DiagnosticQueryError> {
        let operation = "diagnostics query current_by_file";
        match self
            .store
            .current_records_page(
                generation,
                Some(file_occurrence_id),
                request.cursor.as_ref().map(DiagnosticQueryCursor::anchor),
                normalize_limit(request.limit),
            )
            .await
        {
            Ok((records, total, has_more)) => {
                Ok(page_from_bounded_records(records, total, has_more))
            }
            Err(error) => Ok(DiagnosticPage::unavailable(operation, error)),
        }
    }

    /// Stale (superseded or cleared) records bound to `generation`. Stale
    /// findings remain queryable but never re-enter active publication
    /// (Plan 35).
    pub async fn stale_by_generation(
        &self,
        generation: &CodeGenerationId,
        request: &DiagnosticPageRequest,
    ) -> Result<DiagnosticPage, DiagnosticQueryError> {
        let operation = "diagnostics query stale_by_generation";
        match self.store.stale_records(generation).await {
            Ok(records) => Ok(paginate_sorted(records, request)),
            Err(error) => Ok(DiagnosticPage::unavailable(operation, error)),
        }
    }

    /// Stale (superseded or cleared) records for one file occurrence inside
    /// `generation`, paged in ascending anchor order.
    pub async fn stale_by_file(
        &self,
        generation: &CodeGenerationId,
        file_occurrence_id: &FileOccurrenceId,
        request: &DiagnosticPageRequest,
    ) -> Result<DiagnosticPage, DiagnosticQueryError> {
        let operation = "diagnostics query stale_by_file";
        match self.store.stale_records(generation).await {
            Ok(records) => Ok(paginate_sorted(
                records
                    .into_iter()
                    .filter(|record| record.file_occurrence_id == *file_occurrence_id)
                    .collect(),
                request,
            )),
            Err(error) => Ok(DiagnosticPage::unavailable(operation, error)),
        }
    }

    /// Fetches one record by its Plan 13 anchor. A miss is `Complete` with
    /// no record; a store failure is typed `StoreUnavailable`.
    pub async fn by_anchor(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> Result<DiagnosticAnchorLookup, DiagnosticQueryError> {
        let operation = "diagnostics query by_anchor";
        match self.store.record_by_anchor(anchor).await {
            Ok(record) => Ok(DiagnosticAnchorLookup {
                record,
                coverage: DiagnosticQueryCoverage::Complete,
            }),
            Err(error) => Ok(DiagnosticAnchorLookup {
                record: None,
                coverage: DiagnosticQueryCoverage::StoreUnavailable {
                    operation,
                    reason: error.to_string(),
                },
            }),
        }
    }

    /// Forward supersession navigation from `anchor`: the chain walks
    /// `Superseded { successor_generation }` edges toward newer records and
    /// is returned oldest-first including the starting record. The chain
    /// ends at a current, cleared, or missing successor.
    pub async fn supersession_forward(
        &self,
        anchor: &RetrievalAnchorId,
        request: &DiagnosticPageRequest,
    ) -> Result<DiagnosticPage, DiagnosticQueryError> {
        let operation = "diagnostics query supersession_forward";
        match self.store.supersession_chain(anchor).await {
            Ok(chain) => paginate_chain(chain, request),
            Err(error) => Ok(DiagnosticPage::unavailable(operation, error)),
        }
    }

    /// Backward supersession navigation from `anchor`: the chain starts at
    /// the named record and walks toward older records, newest-first, by
    /// inverting the store's forward edges — one step back is the unique
    /// same-key record whose `Superseded { successor_generation }` names the
    /// current record's generation. The walk stops deterministically when
    /// there is no unique predecessor.
    pub async fn supersession_backward(
        &self,
        anchor: &RetrievalAnchorId,
        request: &DiagnosticPageRequest,
    ) -> Result<DiagnosticPage, DiagnosticQueryError> {
        let operation = "diagnostics query supersession_backward";
        let start = match self.store.record_by_anchor(anchor).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Ok(DiagnosticPage {
                    records: Vec::new(),
                    total: 0,
                    coverage: DiagnosticQueryCoverage::Complete,
                    next_cursor: None,
                });
            }
            Err(error) => return Ok(DiagnosticPage::unavailable(operation, error)),
        };
        let key = LogicalFindingKey::of(&start);
        let generations = match self.list_generations().await {
            Ok(generations) => generations,
            Err(error) => return Ok(DiagnosticPage::unavailable(operation, error)),
        };
        let mut same_key_by_generation: BTreeMap<String, Vec<GenerationDiagnosticV1>> =
            BTreeMap::new();
        for generation in &generations {
            let generation_id = match CodeGenerationId::try_from(generation.clone()) {
                Ok(generation_id) => generation_id,
                Err(error) => {
                    return Ok(DiagnosticPage::unavailable(
                        operation,
                        format!("stored generation id {generation}: {error}"),
                    ));
                }
            };
            let records = match self.store.records_for_generation(&generation_id).await {
                Ok(records) => records,
                Err(error) => return Ok(DiagnosticPage::unavailable(operation, error)),
            };
            let matching: Vec<GenerationDiagnosticV1> = records
                .into_iter()
                .filter(|record| LogicalFindingKey::of(record) == key)
                .collect();
            if !matching.is_empty() {
                same_key_by_generation.insert(generation.clone(), matching);
            }
        }

        let mut chain = vec![start.clone()];
        let mut current = start;
        let mut visited: BTreeSet<String> = BTreeSet::new();
        visited.insert(current.diagnostic_anchor.as_str().to_owned());
        loop {
            // Ambiguity guard: the current record must be the unique
            // same-key record of its generation, mirroring the store's
            // forward-walk ambiguity rule.
            let unique_current = same_key_by_generation
                .get(current.generation_id.as_str())
                .is_some_and(|records| records.len() == 1);
            if !unique_current {
                break;
            }
            let predecessors: Vec<&GenerationDiagnosticV1> = same_key_by_generation
                .values()
                .flatten()
                .filter(|record| {
                    matches!(
                        &record.state,
                        DiagnosticRecordStateV1::Superseded {
                            successor_generation
                        } if *successor_generation == current.generation_id
                    )
                })
                .collect();
            if predecessors.len() != 1 {
                break;
            }
            let predecessor = (*predecessors[0]).clone();
            if !visited.insert(predecessor.diagnostic_anchor.as_str().to_owned()) {
                break;
            }
            chain.push(predecessor.clone());
            current = predecessor;
        }
        paginate_chain(chain, request)
    }

    /// Answers "what changed for `file_occurrence_id` between
    /// `from_generation` and `to_generation`" from the store's records and
    /// chains: findings are keyed by the logical finding key, so a finding
    /// republished under a new anchor in `to_generation` lands in the
    /// `superseded` lane, a finding with no `from_generation` counterpart
    /// lands in `introduced`, and a finding with no `to_generation`
    /// counterpart lands in `cleared`. Each lane is capped at `limit`.
    pub async fn generation_file_diff(
        &self,
        from_generation: &CodeGenerationId,
        to_generation: &CodeGenerationId,
        file_occurrence_id: &FileOccurrenceId,
        limit: usize,
    ) -> Result<GenerationDiagnosticDiff, DiagnosticQueryError> {
        let operation = "diagnostics query generation_file_diff";
        if from_generation == to_generation {
            return Err(DiagnosticQueryError::SameGeneration {
                generation: from_generation.clone(),
            });
        }
        let limit = normalize_limit(limit);
        let from_records = match self.store.records_for_generation(from_generation).await {
            Ok(records) => records,
            Err(error) => {
                return Ok(diff_store_unavailable(
                    operation,
                    error,
                    from_generation,
                    to_generation,
                    file_occurrence_id,
                ));
            }
        };
        let to_records = match self.store.records_for_generation(to_generation).await {
            Ok(records) => records,
            Err(error) => {
                return Ok(diff_store_unavailable(
                    operation,
                    error,
                    from_generation,
                    to_generation,
                    file_occurrence_id,
                ));
            }
        };
        // First same-key record wins; store reads are anchor-ordered so this
        // is deterministic.
        let from_by_key = key_file_records(from_records, file_occurrence_id);
        let to_by_key = key_file_records(to_records, file_occurrence_id);

        let mut introduced: Vec<GenerationDiagnosticV1> = Vec::new();
        let mut superseded: Vec<DiagnosticSupersessionPair> = Vec::new();
        let mut cleared: Vec<GenerationDiagnosticV1> = Vec::new();
        for (key, successor) in &to_by_key {
            match from_by_key.get(key) {
                Some(prior) => superseded.push(DiagnosticSupersessionPair {
                    prior: prior.clone(),
                    successor: successor.clone(),
                }),
                None => introduced.push(successor.clone()),
            }
        }
        for (key, prior) in &from_by_key {
            if !to_by_key.contains_key(key) {
                cleared.push(prior.clone());
            }
        }
        introduced.sort_by(anchor_cmp);
        superseded.sort_by(|left, right| anchor_cmp(&left.successor, &right.successor));
        cleared.sort_by(anchor_cmp);

        let mut coverage = DiagnosticQueryCoverage::Complete;
        for lane_len in [introduced.len(), superseded.len(), cleared.len()] {
            if lane_len > limit {
                coverage = DiagnosticQueryCoverage::Truncated;
            }
        }
        introduced.truncate(limit);
        superseded.truncate(limit);
        cleared.truncate(limit);
        Ok(GenerationDiagnosticDiff {
            from_generation: from_generation.clone(),
            to_generation: to_generation.clone(),
            file_occurrence_id: file_occurrence_id.clone(),
            introduced,
            superseded,
            cleared,
            coverage,
        })
    }

    /// Composes the durable current set for `generation` with a dirty
    /// overlay into one deterministic merged view. On the same logical
    /// finding key the overlay entry wins; every entry carries typed
    /// provenance (persisted vs overlay). The overlay lane is session-only
    /// and is never written back (Plan 35).
    pub async fn merged_current_with_overlay(
        &self,
        generation: &CodeGenerationId,
        overlay: &DirtyDiagnosticOverlay,
        request: &DiagnosticPageRequest,
    ) -> Result<MergedDiagnosticView, DiagnosticQueryError> {
        let operation = "diagnostics query merged_current_with_overlay";
        if overlay.clean_generation() != generation {
            return Err(DiagnosticQueryError::OverlayGenerationMismatch {
                overlay_generation: overlay.clean_generation().clone(),
                query_generation: generation.clone(),
            });
        }
        let durable = match self.store.current_records(generation).await {
            Ok(records) => records,
            Err(error) => return Ok(MergedDiagnosticView::unavailable(operation, error)),
        };
        let mut merged: BTreeMap<LogicalFindingKey, MergedDiagnostic> = BTreeMap::new();
        for record in durable {
            merged.insert(
                LogicalFindingKey::of(&record),
                MergedDiagnostic {
                    record,
                    provenance: MergedDiagnosticProvenance::Persisted,
                },
            );
        }
        for entry in overlay.records() {
            merged.insert(
                LogicalFindingKey::of(&entry.record),
                MergedDiagnostic {
                    record: entry.record,
                    provenance: MergedDiagnosticProvenance::Overlay {
                        client_id: entry.client_id,
                        document_uri: entry.document_uri,
                        document_version: entry.document_version,
                    },
                },
            );
        }
        let mut entries: Vec<MergedDiagnostic> = merged.into_values().collect();
        entries.sort_by(|left, right| {
            anchor_cmp(&left.record, &right.record).then_with(|| {
                // Total order on anchor ties: persisted before overlay.
                let rank = |entry: &MergedDiagnostic| match &entry.provenance {
                    MergedDiagnosticProvenance::Persisted => 0u8,
                    MergedDiagnosticProvenance::Overlay { .. } => 1u8,
                };
                rank(left).cmp(&rank(right))
            })
        });
        let (entries, coverage, next_cursor) = paginate_items(
            entries,
            |entry| entry.record.diagnostic_anchor.as_str(),
            request,
        );
        Ok(MergedDiagnosticView {
            entries,
            coverage,
            next_cursor,
        })
    }

    /// Every published generation id, ascending. Read-only probe over the
    /// store's publication ledger used to scope backward chain walks.
    async fn list_generations(&self) -> CrateResult<Vec<String>> {
        let operation = "diagnostics query list_generations";
        let mut rows = self
            .conn
            .query(
                "SELECT generation_id FROM diagnostic_generation_publications \
                 ORDER BY generation_id",
                params![],
            )
            .await
            .map_err(|error| db_error(operation, error))?;
        let mut generations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| db_error(operation, error))?
        {
            generations.push(
                row.get::<String>(0)
                    .map_err(|error| db_error(operation, error))?,
            );
        }
        Ok(generations)
    }
}

fn diff_store_unavailable(
    operation: &'static str,
    error: impl fmt::Display,
    from_generation: &CodeGenerationId,
    to_generation: &CodeGenerationId,
    file_occurrence_id: &FileOccurrenceId,
) -> GenerationDiagnosticDiff {
    GenerationDiagnosticDiff {
        from_generation: from_generation.clone(),
        to_generation: to_generation.clone(),
        file_occurrence_id: file_occurrence_id.clone(),
        introduced: Vec::new(),
        superseded: Vec::new(),
        cleared: Vec::new(),
        coverage: DiagnosticQueryCoverage::StoreUnavailable {
            operation,
            reason: error.to_string(),
        },
    }
}

fn normalize_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_DIAGNOSTIC_PAGE_LIMIT
    } else {
        limit.min(MAX_DIAGNOSTIC_PAGE_LIMIT)
    }
}

fn anchor_cmp(left: &GenerationDiagnosticV1, right: &GenerationDiagnosticV1) -> std::cmp::Ordering {
    left.diagnostic_anchor
        .as_str()
        .cmp(right.diagnostic_anchor.as_str())
}

fn key_file_records(
    records: Vec<GenerationDiagnosticV1>,
    file_occurrence_id: &FileOccurrenceId,
) -> BTreeMap<LogicalFindingKey, GenerationDiagnosticV1> {
    let mut by_key = BTreeMap::new();
    for record in records {
        if record.file_occurrence_id != *file_occurrence_id {
            continue;
        }
        by_key
            .entry(LogicalFindingKey::of(&record))
            .or_insert(record);
    }
    by_key
}

/// Pages a set of items ordered by ascending anchor. The cursor resumes
/// strictly after the anchor it encodes; an anchor that no longer exists
/// still resumes at the first greater anchor, so sorted lanes are total.
fn paginate_items<T: Clone>(
    items: Vec<T>,
    anchor: impl Fn(&T) -> &str,
    request: &DiagnosticPageRequest,
) -> (
    Vec<T>,
    DiagnosticQueryCoverage,
    Option<DiagnosticQueryCursor>,
) {
    let limit = normalize_limit(request.limit);
    let start = match &request.cursor {
        Some(cursor) => items.partition_point(|item| anchor(item) <= cursor.anchor()),
        None => 0,
    };
    let end = (start + limit).min(items.len());
    let page: Vec<T> = items[start..end].to_vec();
    let (coverage, next_cursor) = if end < items.len() {
        let cursor = page
            .last()
            .map(|item| DiagnosticQueryCursor(format!("{CURSOR_PREFIX}{}", anchor(item))));
        (DiagnosticQueryCoverage::Truncated, cursor)
    } else {
        (DiagnosticQueryCoverage::Complete, None)
    };
    (page, coverage, next_cursor)
}

fn paginate_sorted(
    records: Vec<GenerationDiagnosticV1>,
    request: &DiagnosticPageRequest,
) -> DiagnosticPage {
    let total = records.len();
    let (records, coverage, next_cursor) =
        paginate_items(records, |record| record.diagnostic_anchor.as_str(), request);
    DiagnosticPage {
        records,
        total,
        coverage,
        next_cursor,
    }
}

fn page_from_bounded_records(
    records: Vec<GenerationDiagnosticV1>,
    total: usize,
    has_more: bool,
) -> DiagnosticPage {
    let next_cursor = has_more
        .then(|| {
            records
                .last()
                .map(|record| DiagnosticQueryCursor::after_anchor(&record.diagnostic_anchor))
        })
        .flatten();
    DiagnosticPage {
        records,
        total,
        coverage: if has_more {
            DiagnosticQueryCoverage::Truncated
        } else {
            DiagnosticQueryCoverage::Complete
        },
        next_cursor,
    }
}

/// Pages a supersession chain in chain order (not anchor order). The cursor
/// resumes strictly after the chain position it encodes; a cursor whose
/// anchor is not on the chain is a caller error.
fn paginate_chain(
    chain: Vec<GenerationDiagnosticV1>,
    request: &DiagnosticPageRequest,
) -> Result<DiagnosticPage, DiagnosticQueryError> {
    let limit = normalize_limit(request.limit);
    let start = match &request.cursor {
        Some(cursor) => chain
            .iter()
            .position(|record| record.diagnostic_anchor.as_str() == cursor.anchor())
            .map(|position| position + 1)
            .ok_or_else(|| DiagnosticQueryError::InvalidCursor {
                cursor: cursor.encode().to_owned(),
            })?,
        None => 0,
    };
    let end = (start + limit).min(chain.len());
    let page: Vec<GenerationDiagnosticV1> = chain[start..end].to_vec();
    let (coverage, next_cursor) = if end < chain.len() {
        let cursor = page
            .last()
            .map(|record| DiagnosticQueryCursor::after_anchor(&record.diagnostic_anchor));
        (DiagnosticQueryCoverage::Truncated, cursor)
    } else {
        (DiagnosticQueryCoverage::Complete, None)
    };
    Ok(DiagnosticPage {
        records: page,
        total: chain.len(),
        coverage,
        next_cursor,
    })
}

fn db_error(operation: &str, error: impl fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: error.to_string(),
        operation: operation.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
        DiagnosticSeverityV1, SourceSpan, UtcMicros,
    };
    use tracedecay_runtime_core::db::engine::TestConnection;

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

    async fn open_store(path: &std::path::Path) -> TestConnection {
        let conn = TestConnection::open(path);
        DiagnosticsStore::new_runtime(&conn)
            .ensure_schema()
            .await
            .expect("ensure diagnostics schema");
        conn
    }

    const GEN1: &str = "generation.clean.1";
    const GEN2: &str = "generation.clean.2";

    /// Seeds two generations: gen1 publishes A1 (anchor.1, E0308) and B1
    /// (anchor.2, `dead_code`); gen1 is superseded by gen2; gen2 republishes
    /// A1's logical finding as A2 (anchor.3) and adds the new finding C2
    /// (anchor.4, `unused_variables`). B1 has no gen2 successor.
    async fn seed_two_generations(conn: &Connection) {
        let store = DiagnosticsStore::new_runtime(conn);
        store
            .publish_clean_generation(
                &id(GEN1),
                &[
                    fixture_record(GEN1, "anchor.diagnostic.1"),
                    with_message(
                        fixture_record(GEN1, "anchor.diagnostic.2"),
                        "dead_code",
                        "function is never used",
                    ),
                ],
            )
            .await
            .expect("publish gen1");
        store
            .supersede_generation(&id(GEN1), &id(GEN2))
            .await
            .expect("supersede gen1");
        store
            .publish_clean_generation(
                &id(GEN2),
                &[
                    fixture_record(GEN2, "anchor.diagnostic.3"),
                    with_message(
                        fixture_record(GEN2, "anchor.diagnostic.4"),
                        "unused_variables",
                        "unused variable: `tmp`",
                    ),
                ],
            )
            .await
            .expect("publish gen2");
    }

    fn anchors(page: &DiagnosticPage) -> Vec<String> {
        page.records
            .iter()
            .map(|record| record.diagnostic_anchor.as_str().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn current_by_generation_lane_paginates_deterministically() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new(&conn);

        let first = query
            .current_by_generation(&id(GEN2), &DiagnosticPageRequest::new(1, None))
            .await
            .unwrap();
        assert_eq!(anchors(&first), vec!["anchor.diagnostic.3"]);
        assert_eq!(first.total, 2);
        assert_eq!(first.coverage, DiagnosticQueryCoverage::Truncated);
        let cursor = first.next_cursor.clone().expect("truncated page resumes");

        let second = query
            .current_by_generation(&id(GEN2), &DiagnosticPageRequest::new(1, Some(cursor)))
            .await
            .unwrap();
        assert_eq!(anchors(&second), vec!["anchor.diagnostic.4"]);
        assert_eq!(second.total, 2);
        assert_eq!(second.coverage, DiagnosticQueryCoverage::Complete);
        assert!(second.next_cursor.is_none());

        // Determinism: the same walk yields the same anchors again.
        let replay_first = query
            .current_by_generation(&id(GEN2), &DiagnosticPageRequest::new(1, None))
            .await
            .unwrap();
        let replay_second = query
            .current_by_generation(
                &id(GEN2),
                &DiagnosticPageRequest::new(1, replay_first.next_cursor.clone()),
            )
            .await
            .unwrap();
        assert_eq!(anchors(&replay_first), anchors(&first));
        assert_eq!(anchors(&replay_second), anchors(&second));

        // An unbounded read sees the full current set in one complete page.
        let full = query
            .current_by_generation(&id(GEN2), &DiagnosticPageRequest::default())
            .await
            .unwrap();
        assert_eq!(
            anchors(&full),
            vec!["anchor.diagnostic.3", "anchor.diagnostic.4"]
        );
        assert_eq!(full.coverage, DiagnosticQueryCoverage::Complete);
    }

    #[tokio::test]
    async fn current_generation_distinguishes_clean_empty_from_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        DiagnosticsStore::new_runtime(&conn)
            .publish_clean_generation(&id(GEN1), &[])
            .await
            .expect("publish clean empty generation");
        let query = DiagnosticsQuery::new(&conn);
        let current = query.current_generation().await;
        assert_eq!(current.generation, Some(id(GEN1)));
        assert_eq!(current.coverage, DiagnosticQueryCoverage::Complete);

        conn.execute_batch("DROP TABLE diagnostic_generation_publications;")
            .await
            .expect("drop current-generation authority");
        let unavailable = query.current_generation().await;
        assert!(unavailable.generation.is_none());
        assert!(matches!(
            unavailable.coverage,
            DiagnosticQueryCoverage::StoreUnavailable { .. }
        ));
    }

    #[tokio::test]
    async fn current_and_stale_file_lanes_filter_by_file_and_state() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new(&conn);

        let current = query
            .current_by_file(
                &id(GEN2),
                &id("file.occurrence.1"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            anchors(&current),
            vec!["anchor.diagnostic.3", "anchor.diagnostic.4"]
        );
        assert_eq!(current.coverage, DiagnosticQueryCoverage::Complete);
        assert!(
            current
                .records
                .iter()
                .all(|record| record.file_occurrence_id.as_str() == "file.occurrence.1")
        );

        let other_file = query
            .current_by_file(
                &id(GEN2),
                &id("file.occurrence.other"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert!(other_file.records.is_empty());
        assert_eq!(other_file.coverage, DiagnosticQueryCoverage::Complete);

        let stale = query
            .stale_by_generation(&id(GEN1), &DiagnosticPageRequest::default())
            .await
            .unwrap();
        assert_eq!(
            anchors(&stale),
            vec!["anchor.diagnostic.1", "anchor.diagnostic.2"]
        );
        assert!(
            stale
                .records
                .iter()
                .all(|record| !record.state.is_current())
        );

        let stale_file = query
            .stale_by_file(
                &id(GEN1),
                &id("file.occurrence.1"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(anchors(&stale_file), anchors(&stale));
        let stale_other = query
            .stale_by_file(
                &id(GEN1),
                &id("file.occurrence.other"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert!(stale_other.records.is_empty());
    }

    #[tokio::test]
    async fn anchor_lookup_hit_and_miss() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new(&conn);

        let hit = query.by_anchor(&id("anchor.diagnostic.1")).await.unwrap();
        assert_eq!(hit.coverage, DiagnosticQueryCoverage::Complete);
        let record = hit.record.expect("anchor.1 is persisted");
        assert!(matches!(
            &record.state,
            DiagnosticRecordStateV1::Superseded {
                successor_generation
            } if successor_generation.as_str() == GEN2
        ));

        let miss = query
            .by_anchor(&id("anchor.diagnostic.missing"))
            .await
            .unwrap();
        assert_eq!(miss.coverage, DiagnosticQueryCoverage::Complete);
        assert!(miss.record.is_none());
    }

    #[tokio::test]
    async fn supersession_navigation_walks_forward_and_backward() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new(&conn);

        // Forward from the gen1 record crosses into its gen2 successor.
        let forward = query
            .supersession_forward(
                &id("anchor.diagnostic.1"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            anchors(&forward),
            vec!["anchor.diagnostic.1", "anchor.diagnostic.3"]
        );
        assert_eq!(forward.coverage, DiagnosticQueryCoverage::Complete);

        // Backward from the gen2 successor reaches the gen1 record.
        let backward = query
            .supersession_backward(
                &id("anchor.diagnostic.3"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            anchors(&backward),
            vec!["anchor.diagnostic.3", "anchor.diagnostic.1"]
        );
        assert_eq!(backward.coverage, DiagnosticQueryCoverage::Complete);

        // A finding without a successor (or predecessor) is a one-record chain.
        let forward_dead_end = query
            .supersession_forward(
                &id("anchor.diagnostic.2"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(anchors(&forward_dead_end), vec!["anchor.diagnostic.2"]);
        let backward_dead_end = query
            .supersession_backward(
                &id("anchor.diagnostic.4"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(anchors(&backward_dead_end), vec!["anchor.diagnostic.4"]);

        // Chain lanes paginate in chain order with cursor resumption.
        let page = query
            .supersession_forward(
                &id("anchor.diagnostic.1"),
                &DiagnosticPageRequest::new(1, None),
            )
            .await
            .unwrap();
        assert_eq!(anchors(&page), vec!["anchor.diagnostic.1"]);
        assert_eq!(page.coverage, DiagnosticQueryCoverage::Truncated);
        let rest = query
            .supersession_forward(
                &id("anchor.diagnostic.1"),
                &DiagnosticPageRequest::new(1, page.next_cursor.clone()),
            )
            .await
            .unwrap();
        assert_eq!(anchors(&rest), vec!["anchor.diagnostic.3"]);
        assert_eq!(rest.coverage, DiagnosticQueryCoverage::Complete);

        // A cursor from a sorted lane is not valid on a chain lane.
        let sorted_cursor = DiagnosticQueryCursor::decode("dq1:anchor.diagnostic.9").unwrap();
        assert!(matches!(
            query
                .supersession_forward(
                    &id("anchor.diagnostic.1"),
                    &DiagnosticPageRequest::new(1, Some(sorted_cursor)),
                )
                .await,
            Err(DiagnosticQueryError::InvalidCursor { .. })
        ));
    }

    #[tokio::test]
    async fn generation_diff_reports_introduced_superseded_cleared_lanes() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new(&conn);

        let diff = query
            .generation_file_diff(&id(GEN1), &id(GEN2), &id("file.occurrence.1"), 0)
            .await
            .unwrap();
        assert_eq!(diff.coverage, DiagnosticQueryCoverage::Complete);
        assert_eq!(diff.introduced.len(), 1);
        assert_eq!(
            diff.introduced[0].diagnostic_anchor.as_str(),
            "anchor.diagnostic.4"
        );
        assert_eq!(diff.superseded.len(), 1);
        assert_eq!(
            diff.superseded[0].prior.diagnostic_anchor.as_str(),
            "anchor.diagnostic.1"
        );
        assert_eq!(
            diff.superseded[0].successor.diagnostic_anchor.as_str(),
            "anchor.diagnostic.3"
        );
        assert_eq!(diff.cleared.len(), 1);
        assert_eq!(
            diff.cleared[0].diagnostic_anchor.as_str(),
            "anchor.diagnostic.2"
        );

        // Reversing the direction swaps the introduced and cleared lanes.
        let reverse = query
            .generation_file_diff(&id(GEN2), &id(GEN1), &id("file.occurrence.1"), 0)
            .await
            .unwrap();
        assert_eq!(
            reverse.introduced[0].diagnostic_anchor.as_str(),
            "anchor.diagnostic.2"
        );
        assert_eq!(
            reverse.cleared[0].diagnostic_anchor.as_str(),
            "anchor.diagnostic.4"
        );
        assert_eq!(reverse.superseded.len(), 1);

        // A diff needs two distinct generations.
        assert!(matches!(
            query
                .generation_file_diff(&id(GEN1), &id(GEN1), &id("file.occurrence.1"), 0)
                .await,
            Err(DiagnosticQueryError::SameGeneration { .. })
        ));
    }

    #[tokio::test]
    async fn generation_diff_lane_limit_truncates() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new(&conn);

        // Introduce a second new finding in gen2 so the introduced lane
        // exceeds a lane limit of 1.
        let store = DiagnosticsStore::new_runtime(&conn);
        let extra = with_message(
            fixture_record(GEN2, "anchor.diagnostic.5"),
            "unused_imports",
            "unused import",
        );
        store
            .publish_clean_generation(
                &id("generation.clean.3"),
                &[
                    fixture_record("generation.clean.3", "anchor.diagnostic.6"),
                    with_message(
                        fixture_record("generation.clean.3", "anchor.diagnostic.7"),
                        "unused_variables",
                        "unused variable: `tmp`",
                    ),
                    GenerationDiagnosticV1 {
                        generation_id: id("generation.clean.3"),
                        diagnostic_anchor: id("anchor.diagnostic.8"),
                        ..extra
                    },
                ],
            )
            .await
            .expect("publish gen3");

        let diff = query
            .generation_file_diff(
                &id(GEN1),
                &id("generation.clean.3"),
                &id("file.occurrence.1"),
                1,
            )
            .await
            .unwrap();
        assert_eq!(diff.coverage, DiagnosticQueryCoverage::Truncated);
        assert_eq!(diff.introduced.len(), 1);
        assert_eq!(diff.superseded.len(), 1);
        assert_eq!(diff.cleared.len(), 1);
        assert_eq!(
            diff.introduced[0].diagnostic_anchor.as_str(),
            "anchor.diagnostic.7"
        );
    }

    #[tokio::test]
    async fn overlay_merge_prefers_overlay_and_marks_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new(&conn);

        let mut overlay = DirtyDiagnosticOverlay::new(id(GEN2));
        // Same logical finding key as the durable gen2 record anchor.3, under
        // a new overlay anchor: the overlay must win the merge.
        let overlay_override = fixture_record(GEN2, "anchor.overlay.1");
        // A finding with no durable counterpart.
        let overlay_only = with_message(
            fixture_record(GEN2, "anchor.overlay.2"),
            "unused_mut",
            "variable does not need to be mutable",
        );
        overlay
            .replace_document(
                "client.a",
                "file:///src/main.rs",
                3,
                vec![overlay_override.clone(), overlay_only.clone()],
            )
            .expect("overlay accepts current gen2 records");

        let merged = query
            .merged_current_with_overlay(&id(GEN2), &overlay, &DiagnosticPageRequest::default())
            .await
            .unwrap();
        assert_eq!(merged.coverage, DiagnosticQueryCoverage::Complete);
        let by_anchor: BTreeMap<&str, &MergedDiagnostic> = merged
            .entries
            .iter()
            .map(|entry| (entry.record.diagnostic_anchor.as_str(), entry))
            .collect();
        // The durable anchor.3 is displaced by the overlay override.
        assert!(!by_anchor.contains_key("anchor.diagnostic.3"));
        assert_eq!(by_anchor.len(), 3);
        assert!(matches!(
            by_anchor["anchor.diagnostic.4"].provenance,
            MergedDiagnosticProvenance::Persisted
        ));
        assert!(matches!(
            &by_anchor["anchor.overlay.1"].provenance,
            MergedDiagnosticProvenance::Overlay {
                client_id,
                document_version,
                ..
            } if client_id == "client.a" && *document_version == 3
        ));
        assert!(matches!(
            by_anchor["anchor.overlay.2"].provenance,
            MergedDiagnosticProvenance::Overlay { .. }
        ));

        // Deterministic ordering: ascending anchor of the winning record.
        let anchors: Vec<&str> = merged
            .entries
            .iter()
            .map(|entry| entry.record.diagnostic_anchor.as_str())
            .collect();
        assert_eq!(
            anchors,
            vec![
                "anchor.diagnostic.4",
                "anchor.overlay.1",
                "anchor.overlay.2"
            ]
        );

        // Dropping the overlay reverts to the durable set: nothing persisted.
        let durable_only = query
            .merged_current_with_overlay(
                &id(GEN2),
                &DirtyDiagnosticOverlay::new(id(GEN2)),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(durable_only.entries.len(), 2);
        assert!(
            durable_only
                .entries
                .iter()
                .all(|entry| entry.provenance == MergedDiagnosticProvenance::Persisted)
        );

        // An overlay bound to another generation is a caller error.
        let foreign = DirtyDiagnosticOverlay::new(id(GEN1));
        assert!(matches!(
            query
                .merged_current_with_overlay(&id(GEN2), &foreign, &DiagnosticPageRequest::default())
                .await,
            Err(DiagnosticQueryError::OverlayGenerationMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn store_unavailable_is_typed_never_silent() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        // Drive the store-error path by dropping the schema out from under
        // the reader: every store query now fails.
        conn.execute_batch(
            "DROP TABLE generation_diagnostics;
             DROP TABLE diagnostic_generation_publications;",
        )
        .await
        .expect("drop schema to simulate an unavailable store");
        let query = DiagnosticsQuery::new(&conn);

        let is_unavailable = |coverage: &DiagnosticQueryCoverage| {
            matches!(coverage, DiagnosticQueryCoverage::StoreUnavailable { .. })
        };

        let page = query
            .current_by_generation(&id(GEN2), &DiagnosticPageRequest::default())
            .await
            .unwrap();
        assert!(page.records.is_empty());
        assert!(is_unavailable(&page.coverage));

        let page = query
            .current_by_file(
                &id(GEN2),
                &id("file.occurrence.1"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert!(is_unavailable(&page.coverage));

        let page = query
            .stale_by_generation(&id(GEN1), &DiagnosticPageRequest::default())
            .await
            .unwrap();
        assert!(is_unavailable(&page.coverage));

        let page = query
            .stale_by_file(
                &id(GEN1),
                &id("file.occurrence.1"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert!(is_unavailable(&page.coverage));

        let lookup = query.by_anchor(&id("anchor.diagnostic.1")).await.unwrap();
        assert!(lookup.record.is_none());
        assert!(is_unavailable(&lookup.coverage));

        let forward = query
            .supersession_forward(
                &id("anchor.diagnostic.1"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert!(is_unavailable(&forward.coverage));

        let backward = query
            .supersession_backward(
                &id("anchor.diagnostic.3"),
                &DiagnosticPageRequest::default(),
            )
            .await
            .unwrap();
        assert!(is_unavailable(&backward.coverage));

        let diff = query
            .generation_file_diff(&id(GEN1), &id(GEN2), &id("file.occurrence.1"), 0)
            .await
            .unwrap();
        assert!(is_unavailable(&diff.coverage));

        let overlay = DirtyDiagnosticOverlay::new(id(GEN2));
        let merged = query
            .merged_current_with_overlay(&id(GEN2), &overlay, &DiagnosticPageRequest::default())
            .await
            .unwrap();
        assert!(is_unavailable(&merged.coverage));
    }

    #[test]
    fn cursor_decode_rejects_malformed_input() {
        assert!(matches!(
            DiagnosticQueryCursor::decode("garbage"),
            Err(DiagnosticQueryError::InvalidCursor { .. })
        ));
        assert!(matches!(
            DiagnosticQueryCursor::decode("dq1:"),
            Err(DiagnosticQueryError::InvalidCursor { .. })
        ));
        let cursor = DiagnosticQueryCursor::decode("dq1:anchor.diagnostic.1").unwrap();
        assert_eq!(cursor.encode(), "dq1:anchor.diagnostic.1");
        assert_eq!(cursor.anchor(), "anchor.diagnostic.1");
    }
}

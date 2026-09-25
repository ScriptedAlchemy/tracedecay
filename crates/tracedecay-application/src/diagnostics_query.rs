//! Typed diagnostic query core over [`DiagnosticsStore`].
//!
//! This module is a pure read path: no writes, no repair, no schema changes.
//! Every lane returns domain records with explicit coverage. `Complete`,
//! `Truncated`, or `StoreUnavailable`, so a partial or failed read is never
//! presented as a clean result. All list lanes are bounded by a limit
//! plus an opaque cursor and are deterministic: records page in ascending
//! anchor order.
//!
//! The overlay merge composes a session-only [`DirtyDiagnosticOverlay`] with
//! the durable current set into one deterministic view; the overlay wins on
//! the same logical finding key and every entry is marked with its
//! provenance (persisted vs overlay). Overlay state is never persisted.

use std::collections::BTreeMap;
use std::fmt;

use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, GenerationDiagnosticV1, RetrievalAnchorId,
};

use crate::diagnostics_store::{DiagnosticsStore, DirtyDiagnosticOverlay};
use tracedecay_runtime_core::db::Database;
#[cfg(test)]
use tracedecay_runtime_core::db::engine::Connection;

/// Default page limit when a request carries `limit: 0`.
pub const DEFAULT_DIAGNOSTIC_PAGE_LIMIT: usize = 200;

/// Hard cap on any single page, regardless of the requested limit.
pub const MAX_DIAGNOSTIC_PAGE_LIMIT: usize = 1000;

const CURSOR_PREFIX: &str = "dq1:";

/// Explicit coverage for every diagnostic query lane. A read is either
/// complete, deterministically truncated with a resumption cursor, or
/// unavailable because the store could not answer, never silently partial.
/// Engine status and dropped updates remain visible through typed status.
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

/// Point lookup of one record by its retrieval anchor.
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

/// Where one entry of the merged current view came from. The durable lane
/// and the session-only overlay lane stay typed and separate even after
/// merging; overlay findings are never published as durable LSP
/// diagnostics.
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
/// type, they surface as [`DiagnosticQueryCoverage::StoreUnavailable`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticQueryError {
    /// The cursor is malformed.
    InvalidCursor { cursor: String },
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

/// The logical finding key: (repository, producer, code, file occurrence,
/// span, message digest). A durable record and an overlay entry sharing a
/// key are the same logical finding.
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
/// Owns a guarded diagnostics store and exposes no write path. Production
/// callers retain the canonical database capability for each read.
pub struct DiagnosticsQuery<'a> {
    store: DiagnosticsStore<'a>,
}

impl DiagnosticsQuery<'static> {
    pub fn new(database: Database) -> Self {
        Self {
            store: DiagnosticsStore::new(database),
        }
    }
}

impl<'a> DiagnosticsQuery<'a> {
    #[cfg(test)]
    pub fn new_runtime(conn: &'a Connection) -> Self {
        Self {
            store: DiagnosticsStore::new_runtime(conn),
        }
    }

    /// Reads the clean-generation publication pointer. A completed empty
    /// publication returns `Some(generation)` even when it contains no
    /// findings; no pointer is distinct from a clean result.
    #[hotpath::measure(label = "usecases.diagnostics_query.current_generation", future = true)]
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
    #[hotpath::measure(
        label = "usecases.diagnostics_query.current_by_generation",
        future = true
    )]
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
    #[hotpath::measure(label = "usecases.diagnostics_query.current_by_file", future = true)]
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

    /// Fetches one record by its retrieval anchor. A miss is `Complete` with
    /// no record; a store failure is typed `StoreUnavailable`.
    #[hotpath::measure(label = "usecases.diagnostics_query.by_anchor", future = true)]
    pub async fn by_anchor(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> Result<DiagnosticAnchorLookup, DiagnosticQueryError> {
        let operation = "diagnostics query by_anchor";
        match self.store.record_by_anchor(anchor).await {
            Ok(record) => {
                crate::hotpath_observe::diagnostics_query(usize::from(record.is_some()), 1);
                Ok(DiagnosticAnchorLookup {
                    record,
                    coverage: DiagnosticQueryCoverage::Complete,
                })
            }
            Err(error) => Ok(DiagnosticAnchorLookup {
                record: None,
                coverage: DiagnosticQueryCoverage::StoreUnavailable {
                    operation,
                    reason: error.to_string(),
                },
            }),
        }
    }

    /// Composes the durable current set for `generation` with a dirty
    /// overlay into one deterministic merged view. On the same logical
    /// finding key the overlay entry wins; every entry carries typed
    /// provenance (persisted vs overlay). The overlay lane is session-only
    /// and is never written back.
    #[hotpath::measure(
        label = "usecases.diagnostics_query.merged_current_with_overlay",
        future = true
    )]
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
    crate::hotpath_observe::diagnostics_query(records.len(), total);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
        DiagnosticRecordStateV1, DiagnosticSeverityV1, SourceSpan, UtcMicros,
    };
    use tracedecay_runtime_core::db::engine::TestConnection;

    use tracedecay_domain::test_fixtures::id;

    use tracedecay_domain::test_fixtures::repeated_sha256_text as digest;

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
    /// (anchor.2, `dead_code`); gen2 republishes A1's logical finding as A2
    /// (anchor.3) and adds the new finding C2 (anchor.4, `unused_variables`).
    /// Publishing gen2 clears both gen1 records.
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
        let query = DiagnosticsQuery::new_runtime(&conn);

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
        let query = DiagnosticsQuery::new_runtime(&conn);
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
    async fn current_file_lane_filters_by_file() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new_runtime(&conn);

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
    }

    #[tokio::test]
    async fn anchor_lookup_hit_and_miss() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new_runtime(&conn);

        let hit = query.by_anchor(&id("anchor.diagnostic.1")).await.unwrap();
        assert_eq!(hit.coverage, DiagnosticQueryCoverage::Complete);
        let record = hit.record.expect("anchor.1 is persisted");
        assert!(matches!(
            &record.state,
            DiagnosticRecordStateV1::Cleared {
                cleared_in_generation
            } if cleared_in_generation.as_str() == GEN2
        ));

        let miss = query
            .by_anchor(&id("anchor.diagnostic.missing"))
            .await
            .unwrap();
        assert_eq!(miss.coverage, DiagnosticQueryCoverage::Complete);
        assert!(miss.record.is_none());
    }

    #[tokio::test]
    async fn overlay_merge_prefers_overlay_and_marks_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let conn = open_store(&temp.path().join("diagnostics.db")).await;
        seed_two_generations(&conn).await;
        let query = DiagnosticsQuery::new_runtime(&conn);

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
        let query = DiagnosticsQuery::new_runtime(&conn);

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

        let lookup = query.by_anchor(&id("anchor.diagnostic.1")).await.unwrap();
        assert!(lookup.record.is_none());
        assert!(is_unavailable(&lookup.coverage));

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

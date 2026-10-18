//! Typed diagnostic query core over [`DiagnosticsStore`].
//!
//! This module is a pure read path: no writes, no repair, no schema changes.
//! Every lane returns domain records with explicit coverage. `Complete`,
//! `Truncated`, or `StoreUnavailable`, so a partial or failed read is never
//! presented as a clean result. All list lanes are bounded by a limit
//! plus an opaque cursor and are deterministic: records page in ascending
//! anchor order.

use std::fmt;

use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, GenerationDiagnosticV1, RetrievalAnchorId,
};

use crate::diagnostics_store::DiagnosticsStore;
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

/// Exact clean generation currently eligible for active diagnostic reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentDiagnosticGeneration {
    pub generation: Option<CodeGenerationId>,
    pub coverage: DiagnosticQueryCoverage,
}

/// Caller-side query errors. Store failures are never reported through this
/// type, they surface as [`DiagnosticQueryCoverage::StoreUnavailable`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticQueryError {
    /// The cursor is malformed.
    InvalidCursor { cursor: String },
}

impl fmt::Display for DiagnosticQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCursor { cursor } => {
                write!(formatter, "invalid diagnostic query cursor: {cursor}")
            }
        }
    }
}

impl std::error::Error for DiagnosticQueryError {}

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
    #[tracing::instrument(
        name = "usecases.diagnostics_query.current_generation",
        level = "trace",
        skip_all
    )]
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
    #[tracing::instrument(
        name = "usecases.diagnostics_query.current_by_generation",
        level = "trace",
        skip_all
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
    #[tracing::instrument(
        name = "usecases.diagnostics_query.current_by_file",
        level = "trace",
        skip_all
    )]
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
}

fn normalize_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_DIAGNOSTIC_PAGE_LIMIT
    } else {
        limit.min(MAX_DIAGNOSTIC_PAGE_LIMIT)
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

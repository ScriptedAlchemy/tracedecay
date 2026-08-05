use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row, params};
use serde_json::Value;
use tracedecay_capture::opencode as opencode_capture;
use tracedecay_domain::{
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, ProviderId, RetentionClass, SessionId,
};
use tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1;
use tracedecay_store::ParseOffset;

use crate::admission::HostAdmission;
use crate::observation::{CaptureObservationRequest, ObservationCancellation};
use crate::runtime::host_scan::{HOST_SCAN_WINDOW, HostScanBudget, HostScanEvidence};
use crate::runtime::opencode_frontier::{
    GENERATION_KEY as OPENCODE_GENERATION_FRONTIER_KEY,
    REWRITE_KEY as OPENCODE_REWRITE_FRONTIER_KEY, prepare_generation_rewrite,
    read as read_frontier, write as write_frontier,
};
use crate::runtime::opencode_snapshot::MAX_SNAPSHOT_DATABASE_IO_BYTES;
use crate::runtime::shared::TranscriptScopeMatcher;
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_CAPTURE_UNIT_BYTES, SnapshotAdmissionRecord, SnapshotAdmissionRunner,
    SnapshotCaptureOutcome,
};
use crate::runtime::source::{
    HostProviderCoverage, TranscriptIngestError, TranscriptIngestResult, canonical_framed_sha256,
    persist_host_provider_coverage,
};

const PROVIDER: &str = "opencode";
const MAX_MESSAGES_PER_PASS: usize = 4_096;
pub(super) const MAX_MESSAGES_PER_PAGE: usize = 64;
pub(super) const MAX_PARTS_PER_MESSAGE: usize = 256;
pub(super) const MAX_NATIVE_JSON_BYTES: usize = 1024 * 1024;
pub(super) const MAX_OPENCODE_RECORD_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const MAX_ID_BYTES: i64 = 4 * 1024;
pub(super) const OPENCODE_SQL_FRONTIER_KEY: &str = "host-frontier://opencode/sql-rowid/v1";
pub(super) const OPENCODE_PART_FRONTIER_KEY: &str = "host-frontier://opencode/part-rowid/v1";

#[derive(Clone)]
pub struct OpenCodeSource {
    database_path: PathBuf,
    snapshot_scratch_root: PathBuf,
    scope: OpenCodeSourceScope,
}

#[derive(Clone)]
enum OpenCodeSourceScope {
    Project(PathBuf),
    Profile(Vec<PathBuf>),
}

#[derive(Clone)]
pub(super) struct OpenCodeScanSource {
    pub(super) database_path: PathBuf,
    pub(super) source_path: PathBuf,
    scope: OpenCodeSourceScope,
}

#[derive(Clone, Debug)]
struct OpenCodeRecord {
    session_id: String,
    native_record_id: String,
    order: u64,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OpenCodePageCursor {
    pub(super) after_rowid: i64,
}

#[derive(Clone, Debug)]
pub(super) struct OpenCodeMessageRef {
    pub(super) rowid: i64,
    pub(super) id: String,
    pub(super) session_id: String,
    pub(super) order: u64,
    pub(super) measured_bytes: u64,
    pub(super) max_field_bytes: u64,
    pub(super) part_count: usize,
}

pub(super) struct OpenCodeReferencePage {
    pub(super) references: Vec<OpenCodeMessageRef>,
    pub(super) next: OpenCodePageCursor,
    pub(super) source_complete: bool,
}

#[derive(Clone, Copy)]
enum OpenCodeScanKind {
    Messages,
    Parts,
    Rewrite,
}

impl OpenCodeScanKind {
    const fn frontier_key(self) -> &'static str {
        match self {
            Self::Messages => OPENCODE_SQL_FRONTIER_KEY,
            Self::Parts => OPENCODE_PART_FRONTIER_KEY,
            Self::Rewrite => OPENCODE_REWRITE_FRONTIER_KEY,
        }
    }
}

struct OpenCodeMaterializedPage {
    records: Vec<OpenCodeRecord>,
    input_bytes: u64,
    fully_processed: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OpenCodeCaptureOutcome {
    pub stats: crate::runtime::shared::TranscriptIngestStats,
    pub bytes_consumed: u64,
    pub deferred_by_byte_cap: bool,
    pub scan_cancelled: bool,
    pub scan_input_bound_reached: bool,
    pub scan_non_durable_units: u64,
    pub scan_unavailable_units: u64,
}

impl SnapshotAdmissionRecord for OpenCodeRecord {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn native_record_id(&self) -> &str {
        &self.native_record_id
    }

    fn order(&self) -> u64 {
        self.order
    }

    fn payload(&self) -> &[u8] {
        &self.payload
    }

    fn capture_request(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        cancellation: ObservationCancellation,
    ) -> TranscriptIngestResult<CaptureObservationRequest> {
        let range = ObservationSourceRangeV1::new(self.order, self.order.saturating_add(1))?;
        let native_id = ObservationId::new(&self.native_record_id).map_err(|_| invalid_frame())?;
        let parsed = parse_normalized_observation_record_v1(
            &self.payload,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                opencode_capture::normalize_observation(
                    &native,
                    &self.session_id,
                    native_id.clone(),
                    range,
                )
            },
        )
        .map_err(|_| TranscriptIngestError::NonDurableRecord {
            provider: PROVIDER,
            offset: range.start(),
            end_offset: range.end(),
            reason: "normalized OpenCode record is not durable",
        })?;
        let provider = ProviderId::new(PROVIDER).map_err(|_| invalid_frame())?;
        let session = SessionId::new(&self.session_id).map_err(|_| invalid_frame())?;
        let source = ObservationSourceIdentityV1::for_provider(provider, session)
            .map_err(|_| invalid_frame())?;
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source,
            scope,
            generation,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            native_id,
        )?;
        CaptureObservationRequest::new(
            parsed,
            identity,
            expected_cursor,
            RetentionClass::new("transcript.opencode.v1")?,
            cancellation,
        )
        .map_err(|_| invalid_frame())
    }
}

impl OpenCodeSource {
    pub fn new_for_project(project_root: &Path) -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        let snapshot_scratch_root = crate::runtime::opencode_snapshot::snapshot_scratch_root()?;
        Some(Self::with_database_for_project_and_scratch(
            opencode_data_dir(&home).join("opencode.db"),
            snapshot_scratch_root,
            project_root.to_path_buf(),
        ))
    }

    pub fn new_for_user(roots: Vec<PathBuf>) -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        let snapshot_scratch_root = crate::runtime::opencode_snapshot::snapshot_scratch_root()?;
        Some(Self::with_database_for_user_and_scratch(
            opencode_data_dir(&home).join("opencode.db"),
            snapshot_scratch_root,
            roots,
        ))
    }

    #[cfg(test)]
    pub fn with_database_for_project(database_path: PathBuf, project_root: PathBuf) -> Self {
        let snapshot_scratch_root = database_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("opencode-snapshot-scratch");
        Self::with_database_for_project_and_scratch(
            database_path,
            snapshot_scratch_root,
            project_root,
        )
    }

    fn with_database_for_project_and_scratch(
        database_path: PathBuf,
        snapshot_scratch_root: PathBuf,
        project_root: PathBuf,
    ) -> Self {
        Self {
            database_path,
            snapshot_scratch_root,
            scope: OpenCodeSourceScope::Project(project_root),
        }
    }

    #[cfg(test)]
    pub fn with_database_for_user(database_path: PathBuf, registered_roots: Vec<PathBuf>) -> Self {
        let snapshot_scratch_root = database_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("opencode-snapshot-scratch");
        Self::with_database_for_user_and_scratch(
            database_path,
            snapshot_scratch_root,
            registered_roots,
        )
    }

    fn with_database_for_user_and_scratch(
        database_path: PathBuf,
        snapshot_scratch_root: PathBuf,
        registered_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            database_path,
            snapshot_scratch_root,
            scope: OpenCodeSourceScope::Profile(registered_roots),
        }
    }
}

impl OpenCodeScanSource {
    pub(super) fn scope_matcher(&self) -> TranscriptScopeMatcher {
        match &self.scope {
            OpenCodeSourceScope::Project(root) => TranscriptScopeMatcher::project(root),
            OpenCodeSourceScope::Profile(roots) => TranscriptScopeMatcher::profile(roots),
        }
    }
}

pub(crate) async fn capture_opencode_observations(
    facade: &dyn HostAdmission,
    source: &OpenCodeSource,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<OpenCodeCaptureOutcome> {
    let snapshot_budget = HostScanBudget::new(
        MAX_SNAPSHOT_DATABASE_IO_BYTES,
        usize::MAX,
        Instant::now() + HOST_SCAN_WINDOW,
        cancellation.clone(),
    );
    let snapshot_attempt = crate::runtime::opencode_snapshot::snapshot_database(
        source.database_path.clone(),
        source.snapshot_scratch_root.clone(),
        snapshot_budget,
    )
    .await?;
    let (snapshot, snapshot_budget) = snapshot_attempt;
    let Some(snapshot) = snapshot else {
        let outcome = outcome_for_scan_evidence(snapshot_budget.evidence());
        persist_host_provider_coverage(
            facade,
            &scope,
            PROVIDER,
            HostProviderCoverage::Unavailable,
            1,
        )
        .await?;
        return Ok(outcome);
    };
    let scan_source = OpenCodeScanSource {
        database_path: snapshot.path.clone(),
        source_path: source.database_path.clone(),
        scope: source.scope.clone(),
    };
    let max_input_bytes = max_new_bytes
        .unwrap_or(MAX_SNAPSHOT_CAPTURE_UNIT_BYTES)
        .min(MAX_SNAPSHOT_CAPTURE_UNIT_BYTES);
    let mut scan_budget = HostScanBudget::new(
        max_input_bytes,
        MAX_MESSAGES_PER_PASS,
        Instant::now() + HOST_SCAN_WINDOW,
        cancellation.clone(),
    );
    let mut runner = SnapshotAdmissionRunner::new(max_new_bytes);
    let current_generation = snapshot.generation.generation_id();
    let (mut generation_frontier, mut rewrite_frontier) = prepare_generation_rewrite(
        facade,
        &scope,
        current_generation,
        snapshot.source_file_identity,
    )
    .await?;
    for scan_kind in [
        OpenCodeScanKind::Messages,
        OpenCodeScanKind::Parts,
        OpenCodeScanKind::Rewrite,
    ] {
        if !scan_budget.checkpoint() {
            break;
        }
        if matches!(scan_kind, OpenCodeScanKind::Rewrite)
            && rewrite_frontier.byte_offset == u64::MAX
        {
            continue;
        }
        let stored_frontier = if matches!(scan_kind, OpenCodeScanKind::Rewrite) {
            rewrite_frontier
        } else {
            read_frontier(facade, &scope, scan_kind.frontier_key()).await?
        };
        let initialize_part_frontier = matches!(scan_kind, OpenCodeScanKind::Parts)
            && stored_frontier.file_id != snapshot.source_file_identity;
        let mut durable_frontier = if stored_frontier.file_id == snapshot.source_file_identity {
            stored_frontier
        } else {
            ParseOffset {
                byte_offset: 0,
                mtime: stored_frontier.mtime,
                file_id: snapshot.source_file_identity,
            }
        };
        let mut cursor = OpenCodePageCursor {
            after_rowid: i64::try_from(durable_frontier.byte_offset)
                .map_err(|_| invalid_frame())?,
        };
        loop {
            if !scan_budget.checkpoint() {
                break;
            }
            let owned_source = scan_source.clone();
            let scan = tokio::task::spawn_blocking(move || {
                scan_reference_page(&owned_source, scan_kind, cursor, scan_budget)
            })
            .await
            .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
            let (page, mut returned_budget) = scan;
            let previous_cursor = cursor;
            cursor = page.next;
            let mut page_fully_processed = returned_budget.checkpoint();
            if page_fully_processed && !page.references.is_empty() && !initialize_part_frontier {
                let owned_source = scan_source.clone();
                let materialized = tokio::task::spawn_blocking(move || {
                    materialize_reference_page(&owned_source, page.references, returned_budget)
                })
                .await
                .map_err(|_| {
                    TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER }
                })??;
                let (materialized, budget) = materialized;
                returned_budget = budget;
                page_fully_processed = materialized.fully_processed;
                runner
                    .admit_batch(
                        facade,
                        materialized.input_bytes,
                        &scope,
                        cancellation,
                        || Ok(Some((snapshot.generation, materialized.records))),
                    )
                    .await?;
            }
            if page_fully_processed && cursor != previous_cursor {
                durable_frontier = ParseOffset {
                    byte_offset: u64::try_from(cursor.after_rowid).map_err(|_| invalid_frame())?,
                    mtime: durable_frontier.mtime.saturating_add(1),
                    file_id: snapshot.source_file_identity,
                };
                facade
                    .advance_parse_offset(&scope, scan_kind.frontier_key(), durable_frontier)
                    .await
                    .map_err(|outcome| {
                        crate::runtime::snapshot_observation::host_admission_error(
                            PROVIDER, outcome,
                        )
                    })?;
                if matches!(scan_kind, OpenCodeScanKind::Rewrite) {
                    rewrite_frontier = durable_frontier;
                }
            }
            if page_fully_processed
                && page.source_complete
                && matches!(scan_kind, OpenCodeScanKind::Rewrite)
            {
                if generation_frontier.byte_offset == current_generation {
                    rewrite_frontier = ParseOffset {
                        byte_offset: u64::MAX,
                        ..durable_frontier
                    };
                    write_frontier(
                        facade,
                        &scope,
                        OPENCODE_REWRITE_FRONTIER_KEY,
                        rewrite_frontier,
                    )
                    .await?;
                } else {
                    let revision = generation_frontier
                        .mtime
                        .max(rewrite_frontier.mtime)
                        .saturating_add(1)
                        .max(1);
                    generation_frontier = ParseOffset {
                        byte_offset: current_generation,
                        mtime: revision,
                        file_id: snapshot.source_file_identity,
                    };
                    rewrite_frontier = ParseOffset {
                        byte_offset: 0,
                        mtime: revision,
                        file_id: snapshot.source_file_identity,
                    };
                    write_frontier(
                        facade,
                        &scope,
                        OPENCODE_GENERATION_FRONTIER_KEY,
                        generation_frontier,
                    )
                    .await?;
                    write_frontier(
                        facade,
                        &scope,
                        OPENCODE_REWRITE_FRONTIER_KEY,
                        rewrite_frontier,
                    )
                    .await?;
                }
            }
            scan_budget = returned_budget;
            if page.source_complete
                || cursor == previous_cursor
                || !page_fully_processed
                || scan_budget.evidence().unit_bound_reached
                || !scan_budget.checkpoint()
            {
                break;
            }
        }
    }
    if scan_budget.evidence().is_deferred() {
        runner.defer();
    }
    let evidence = scan_budget.evidence();
    let SnapshotCaptureOutcome {
        stats,
        bytes_consumed,
        deferred_by_byte_cap,
    } = runner.finish();
    let outcome = OpenCodeCaptureOutcome {
        stats,
        bytes_consumed,
        deferred_by_byte_cap,
        scan_cancelled: evidence.cancelled || snapshot_budget.evidence().cancelled,
        scan_input_bound_reached: evidence.input_bound_reached
            || snapshot_budget.evidence().input_bound_reached,
        scan_non_durable_units: evidence
            .non_durable_units
            .saturating_add(snapshot_budget.evidence().non_durable_units),
        scan_unavailable_units: evidence
            .unavailable_units
            .saturating_add(snapshot_budget.evidence().unavailable_units),
    };
    let deferred_units = outcome
        .scan_non_durable_units
        .saturating_add(outcome.scan_unavailable_units)
        .saturating_add(u64::from(
            outcome.deferred_by_byte_cap
                || outcome.scan_cancelled
                || outcome.scan_input_bound_reached,
        ));
    if !outcome.scan_cancelled {
        persist_host_provider_coverage(
            facade,
            &scope,
            PROVIDER,
            if deferred_units > 0 {
                HostProviderCoverage::Partial
            } else {
                HostProviderCoverage::Complete
            },
            deferred_units,
        )
        .await?;
    }
    Ok(outcome)
}

fn scan_reference_page(
    source: &OpenCodeScanSource,
    scan_kind: OpenCodeScanKind,
    cursor: OpenCodePageCursor,
    budget: HostScanBudget,
) -> TranscriptIngestResult<(OpenCodeReferencePage, HostScanBudget)> {
    match scan_kind {
        OpenCodeScanKind::Messages => scan_message_reference_page(source, cursor, budget),
        OpenCodeScanKind::Parts => {
            crate::runtime::opencode_part_scan::scan_part_reference_page(source, cursor, budget)
        }
        OpenCodeScanKind::Rewrite => scan_message_reference_page(source, cursor, budget),
    }
}

fn scan_message_reference_page(
    source: &OpenCodeScanSource,
    cursor: OpenCodePageCursor,
    mut budget: HostScanBudget,
) -> TranscriptIngestResult<(OpenCodeReferencePage, HostScanBudget)> {
    let Some(connection) = open_scan_connection(source, &mut budget)? else {
        return Ok((
            OpenCodeReferencePage {
                references: Vec::new(),
                next: cursor,
                source_complete: true,
            },
            budget,
        ));
    };
    install_progress_handler(&connection, &source.source_path, &budget)?;
    let matcher = source.scope_matcher();
    let mut statement = connection
        .prepare(
            "SELECT m.rowid,
                    CASE WHEN length(m.id) <= ?1 THEN m.id ELSE NULL END AS message_id,
                    CASE WHEN length(m.session_id) <= ?1 THEN m.session_id ELSE NULL END
                        AS session_id,
                    CASE WHEN length(s.directory) <= ?1 THEN s.directory ELSE NULL END
                        AS directory,
                    length(m.data) AS message_bytes,
                    COALESCE((
                        SELECT SUM(length(p.data)) FROM part p WHERE p.message_id = m.id
                    ), 0) AS part_bytes,
                    COALESCE((
                        SELECT MAX(length(p.data)) FROM part p WHERE p.message_id = m.id
                    ), 0) AS max_part_bytes,
                    (
                        SELECT COUNT(*) FROM part p WHERE p.message_id = m.id
                    ) AS part_count,
                    (
                        SELECT COUNT(*) - 1
                        FROM message ordered
                        WHERE ordered.session_id = m.session_id
                          AND ordered.rowid <= m.rowid
                    ) AS source_order
             FROM message m
             JOIN session s ON s.id = m.session_id
             WHERE m.rowid > ?2
             ORDER BY m.rowid
             LIMIT ?3",
        )
        .map_err(|error| {
            scan_error(
                "prepare message reference query",
                &source.source_path,
                error,
            )
        })?;
    let mut rows = statement
        .query(params![
            MAX_ID_BYTES,
            cursor.after_rowid,
            i64::try_from(MAX_MESSAGES_PER_PAGE).map_err(|_| invalid_frame())?
        ])
        .map_err(|error| scan_error("query message references", &source.source_path, error))?;
    let mut references = Vec::with_capacity(MAX_MESSAGES_PER_PAGE);
    let mut rows_seen = 0_usize;
    let mut after_rowid = cursor.after_rowid;
    let mut query_exhausted = false;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => {
                query_exhausted = true;
                break;
            }
            Err(error) => {
                if !budget.checkpoint() {
                    break;
                }
                return Err(scan_error(
                    "read message reference",
                    &source.source_path,
                    error,
                ));
            }
        };
        if !budget.try_charge_unit() {
            break;
        }
        rows_seen = rows_seen.saturating_add(1);
        after_rowid = row
            .get(0)
            .map_err(|error| scan_error("decode message rowid", &source.source_path, error))?;
        let id = sql_text(row, 1, &source.source_path, "decode message id")?;
        let session_id = sql_text(row, 2, &source.source_path, "decode session id")?;
        let directory = sql_text(row, 3, &source.source_path, "decode session directory")?;
        let message_bytes = row
            .get::<_, Option<i64>>(4)
            .map_err(|error| scan_error("decode message length", &source.source_path, error))?;
        let part_bytes = row
            .get::<_, i64>(5)
            .map_err(|error| scan_error("decode part length", &source.source_path, error))?;
        let part_count = row
            .get::<_, i64>(7)
            .map_err(|error| scan_error("decode part count", &source.source_path, error))?;
        let order = row
            .get::<_, i64>(8)
            .map_err(|error| scan_error("decode message order", &source.source_path, error))?;
        let max_part_bytes = row.get::<_, i64>(6).map_err(|error| {
            scan_error("decode maximum part length", &source.source_path, error)
        })?;
        let (Some(id), Some(session_id), Some(directory), Some(message_bytes)) =
            (id, session_id, directory, message_bytes)
        else {
            budget.mark_non_durable();
            continue;
        };
        if !matcher.accepts(Some(Path::new(&directory))) {
            continue;
        }
        let Ok(message_bytes) = u64::try_from(message_bytes) else {
            budget.mark_non_durable();
            continue;
        };
        let Ok(part_bytes) = u64::try_from(part_bytes) else {
            budget.mark_non_durable();
            continue;
        };
        let Ok(part_count) = usize::try_from(part_count) else {
            budget.mark_non_durable();
            continue;
        };
        let Ok(max_part_bytes) = u64::try_from(max_part_bytes) else {
            budget.mark_non_durable();
            continue;
        };
        let Ok(order) = u64::try_from(order) else {
            budget.mark_non_durable();
            continue;
        };
        references.push(OpenCodeMessageRef {
            rowid: after_rowid,
            id,
            session_id,
            order,
            measured_bytes: message_bytes.saturating_add(part_bytes),
            max_field_bytes: message_bytes.max(max_part_bytes),
            part_count,
        });
    }
    let source_complete = query_exhausted && rows_seen < MAX_MESSAGES_PER_PAGE;
    let next = OpenCodePageCursor { after_rowid };
    Ok((
        OpenCodeReferencePage {
            references,
            next,
            source_complete,
        },
        budget,
    ))
}

fn materialize_reference_page(
    source: &OpenCodeScanSource,
    references: Vec<OpenCodeMessageRef>,
    mut budget: HostScanBudget,
) -> TranscriptIngestResult<(OpenCodeMaterializedPage, HostScanBudget)> {
    let Some(connection) = open_scan_connection(source, &mut budget)? else {
        budget.mark_unavailable();
        return Ok((
            OpenCodeMaterializedPage {
                records: Vec::new(),
                input_bytes: 0,
                fully_processed: false,
            },
            budget,
        ));
    };
    install_progress_handler(&connection, &source.source_path, &budget)?;
    let max_native_json_bytes =
        u64::try_from(MAX_NATIVE_JSON_BYTES).map_err(|_| invalid_frame())?;
    let before = budget.consumed_input_bytes();
    let mut records = Vec::with_capacity(references.len());
    let mut fully_processed = true;
    for reference in references {
        if !budget.checkpoint() {
            fully_processed = false;
            break;
        }
        if reference.part_count > MAX_PARTS_PER_MESSAGE
            || reference.measured_bytes > MAX_OPENCODE_RECORD_BYTES
            || reference.max_field_bytes > max_native_json_bytes
        {
            let _ = budget.try_charge_input(reference.measured_bytes);
            budget.mark_non_durable();
            continue;
        }
        if !budget.try_charge_input(reference.measured_bytes) {
            fully_processed = false;
            break;
        }
        match load_record(&connection, &reference, &source.source_path)? {
            Some(record) => records.push(record),
            None => budget.mark_non_durable(),
        }
    }
    Ok((
        OpenCodeMaterializedPage {
            records,
            input_bytes: budget.consumed_input_bytes().saturating_sub(before),
            fully_processed,
        },
        budget,
    ))
}

fn load_record(
    connection: &Connection,
    reference: &OpenCodeMessageRef,
    database_path: &Path,
) -> TranscriptIngestResult<Option<OpenCodeRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN length(data) <= ?1 THEN data ELSE NULL END
             FROM message WHERE rowid = ?2",
        )
        .map_err(|error| scan_error("prepare message payload query", database_path, error))?;
    let mut rows = statement
        .query(params![
            i64::try_from(MAX_NATIVE_JSON_BYTES).map_err(|_| invalid_frame())?,
            reference.rowid
        ])
        .map_err(|error| scan_error("query message payload", database_path, error))?;
    let Some(row) = rows
        .next()
        .map_err(|error| scan_error("read message payload", database_path, error))?
    else {
        return Ok(None);
    };
    let data = sql_bytes(row, 0, database_path, "decode message data")?;
    let Some(data) = data else {
        return Ok(None);
    };
    let Ok(mut message) = serde_json::from_slice::<Value>(&data) else {
        return Ok(None);
    };
    let Value::Object(message_fields) = &mut message else {
        return Ok(None);
    };
    message_fields
        .entry("id")
        .or_insert_with(|| Value::String(reference.id.clone()));
    message_fields
        .entry("sessionID")
        .or_insert_with(|| Value::String(reference.session_id.clone()));
    let parts = load_parts(connection, &reference.id, database_path)?;
    if parts.deferred {
        return Ok(None);
    }
    let payload = serde_json::to_vec(&serde_json::json!({
        "message": message,
        "parts": parts.values,
    }))
    .map_err(|_| invalid_frame())?;
    if payload.len() as u64 > MAX_OPENCODE_RECORD_BYTES {
        return Ok(None);
    }
    Ok(Some(OpenCodeRecord {
        session_id: reference.session_id.clone(),
        native_record_id: stable_native_id(&reference.id, &payload),
        order: reference.order,
        payload,
    }))
}

struct LoadedParts {
    values: Vec<Value>,
    deferred: bool,
}

fn load_parts(
    connection: &Connection,
    message_id: &str,
    database_path: &Path,
) -> TranscriptIngestResult<LoadedParts> {
    let mut statement = connection
        .prepare(
            "SELECT id, length(data),
                    CASE WHEN length(data) <= ?1 THEN data ELSE NULL END
             FROM part
             WHERE message_id = ?2
             ORDER BY id
             LIMIT ?3",
        )
        .map_err(|error| scan_error("prepare part query", database_path, error))?;
    let mut query = statement
        .query(params![
            i64::try_from(MAX_NATIVE_JSON_BYTES).map_err(|_| invalid_frame())?,
            message_id,
            i64::try_from(MAX_PARTS_PER_MESSAGE.saturating_add(1)).map_err(|_| invalid_frame())?
        ])
        .map_err(|error| scan_error("query parts", database_path, error))?;
    let mut values = Vec::new();
    let mut deferred = false;
    while let Some(row) = query
        .next()
        .map_err(|error| scan_error("read part row", database_path, error))?
    {
        if values.len() >= MAX_PARTS_PER_MESSAGE {
            deferred = true;
            break;
        }
        let Some(id) = sql_text(row, 0, database_path, "decode part id")? else {
            deferred = true;
            continue;
        };
        let data = sql_bytes(row, 2, database_path, "decode part data")?;
        let Some(data) = data else {
            deferred = true;
            continue;
        };
        let Ok(mut value) = serde_json::from_slice::<Value>(&data) else {
            deferred = true;
            continue;
        };
        if let Value::Object(fields) = &mut value {
            fields.entry("id").or_insert(Value::String(id));
        }
        values.push(value);
    }
    Ok(LoadedParts { values, deferred })
}

pub(super) fn open_scan_connection(
    source: &OpenCodeScanSource,
    budget: &mut HostScanBudget,
) -> TranscriptIngestResult<Option<Connection>> {
    if !budget.checkpoint() {
        return Ok(None);
    }
    tracedecay_rusqlite_runtime::open_immutable_reader(&source.database_path)
        .map(Some)
        .map_err(|error| scan_error("open immutable database", &source.source_path, error))
}

pub(super) fn install_progress_handler(
    connection: &Connection,
    database_path: &Path,
    budget: &HostScanBudget,
) -> TranscriptIngestResult<()> {
    let cancellation = budget.cancellation();
    let deadline = budget.deadline();
    connection
        .progress_handler(
            1_000,
            Some(move || cancellation.is_cancelled() || Instant::now() >= deadline),
        )
        .map_err(|error| {
            scan_error(
                "install cancellation progress handler",
                database_path,
                error,
            )
        })
}

fn stable_native_id(native: &str, payload: &[u8]) -> String {
    format!(
        "opencode.message.{}",
        canonical_framed_sha256(
            b"tracedecay.opencode.message-content.v1",
            &[native.as_bytes(), payload],
        )
    )
}

fn sql_bytes(
    row: &Row<'_>,
    index: usize,
    database_path: &Path,
    operation: &'static str,
) -> TranscriptIngestResult<Option<Vec<u8>>> {
    let value = row
        .get_ref(index)
        .map_err(|error| scan_error(operation, database_path, error))?;
    match value {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Ok(Some(bytes.to_vec())),
        _ => Ok(None),
    }
}

pub(super) fn sql_text(
    row: &Row<'_>,
    index: usize,
    database_path: &Path,
    operation: &'static str,
) -> TranscriptIngestResult<Option<String>> {
    let value = row
        .get_ref(index)
        .map_err(|error| scan_error(operation, database_path, error))?;
    match value {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map(Some)
            .map_err(|error| scan_error(operation, database_path, error)),
        _ => Ok(None),
    }
}

fn outcome_for_scan_evidence(evidence: HostScanEvidence) -> OpenCodeCaptureOutcome {
    OpenCodeCaptureOutcome {
        deferred_by_byte_cap: evidence.is_deferred(),
        scan_cancelled: evidence.cancelled,
        scan_input_bound_reached: evidence.input_bound_reached,
        scan_non_durable_units: evidence.non_durable_units,
        scan_unavailable_units: evidence.unavailable_units,
        ..OpenCodeCaptureOutcome::default()
    }
}

fn opencode_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home.join("Library/Application Support/opencode");
    }
    #[cfg(target_os = "windows")]
    {
        return home.join("AppData/Local/opencode");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".local/share/opencode")
    }
}

fn scan_error(
    operation: &'static str,
    path: &Path,
    error: impl std::error::Error + Send + Sync + 'static,
) -> TranscriptIngestError {
    TranscriptIngestError::ScanIo {
        operation,
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    }
}

const fn invalid_frame() -> TranscriptIngestError {
    TranscriptIngestError::InvalidFrameState { provider: PROVIDER }
}

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;

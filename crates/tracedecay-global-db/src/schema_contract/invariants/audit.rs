use std::collections::BTreeSet;

use futures_util::future::try_join_all;
use tracedecay_domain::DurableObservationV1;
use tracedecay_store::{
    ObservationProjection, ProjectionSkipReason, SESSION_MESSAGE_PROJECTOR_VERSION,
    SessionMessageProjection, WorkflowFactProjection,
};

use crate::global_db_operation_error;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::rows::{authority_violation, decode_authority_json};
use super::{AUDIT_PAGE_ROWS, INCOMPLETE_EXHAUSTIVE_PASS, OPERATION, projection_checkpoint};
const AUDIT_NAME: &str = "observation-authority";

const AUDIT_VERSION: i64 = 2;
pub(super) const MAX_BOUNDED_AUDIT_PASSES: i64 = 64;
const DETAILED_AUDIT_CONCURRENCY: usize = 32;
const DETAILED_TAIL_CONCURRENCY: usize = 1;
// Amortize the page query across several bounded validation chunks while
// checkpointing often enough to stay below one ordinary statement deadline.
const DETAILED_AUDIT_CHUNKS_PER_PAGE: usize = 3;
const MAX_DETAILED_OBSERVATIONS_PER_PAGE: usize =
    DETAILED_AUDIT_CONCURRENCY * DETAILED_AUDIT_CHUNKS_PER_PAGE;
const PROJECTION_PROGRESS_PAGE_INTERVAL: i64 = 1;

#[derive(Clone, Copy, Default)]
pub(super) struct AuditCheckpoint {
    pub(super) receipt_rowid: i64,
    pub(super) observation_sequence: i64,
    pub(super) source_cursor_rowid: i64,
    pub(super) source_advance_rowid: i64,
    pub(super) provenance_rowid: i64,
    pub(super) disposition_rowid: i64,
    pub(super) alias_rowid: i64,
    pub(super) projection_checkpoint: i64,
    pub(super) bounded_passes_since_exhaustive: i64,
}

pub(super) struct AuditProgress {
    pub(super) checkpoint: AuditCheckpoint,
    pub(super) receipts_audited: i64,
    pub(super) observations_audited: i64,
    pub(super) provenance_audited: i64,
    pub(super) dispositions_audited: i64,
    pub(super) aliases_audited: i64,
}

pub(super) async fn ensure_audit_checkpoint_schema(
    conn: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_existing_audit_checkpoint_baseline(conn).await?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS authority_audit_checkpoints (
            audit_name TEXT PRIMARY KEY,
            audit_version INTEGER NOT NULL,
            receipt_rowid INTEGER NOT NULL,
            observation_sequence INTEGER NOT NULL,
            source_cursor_rowid INTEGER NOT NULL DEFAULT 0,
            source_advance_rowid INTEGER NOT NULL DEFAULT 0,
            provenance_rowid INTEGER NOT NULL,
            disposition_rowid INTEGER NOT NULL,
            alias_rowid INTEGER NOT NULL,
            projection_checkpoint INTEGER NOT NULL,
            last_receipts_audited INTEGER NOT NULL,
            last_observations_audited INTEGER NOT NULL,
            last_provenance_audited INTEGER NOT NULL,
            last_dispositions_audited INTEGER NOT NULL,
            last_aliases_audited INTEGER NOT NULL,
            bounded_passes_since_exhaustive INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS authority_foreign_key_audit_progress (
            audit_name TEXT PRIMARY KEY,
            last_table TEXT NOT NULL
        );",
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut rows = conn
        .query(
            "SELECT 1 FROM pragma_table_xinfo('authority_audit_checkpoints')
             WHERE name = 'bounded_passes_since_exhaustive'",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let has_bounded_passes = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .is_some();
    drop(rows);
    if !has_bounded_passes {
        conn.execute(
            "ALTER TABLE authority_audit_checkpoints
             ADD COLUMN bounded_passes_since_exhaustive INTEGER NOT NULL DEFAULT 0",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_xinfo('authority_audit_checkpoints')
             WHERE name IN ('source_cursor_rowid', 'source_advance_rowid')",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut has_source_cursor_rowid = false;
    let mut has_source_advance_rowid = false;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        match row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .as_str()
        {
            "source_cursor_rowid" => has_source_cursor_rowid = true,
            "source_advance_rowid" => has_source_advance_rowid = true,
            _ => {}
        }
    }
    drop(rows);
    if !has_source_cursor_rowid {
        conn.execute(
            "ALTER TABLE authority_audit_checkpoints
             ADD COLUMN source_cursor_rowid INTEGER NOT NULL DEFAULT 0",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    if !has_source_advance_rowid {
        conn.execute(
            "ALTER TABLE authority_audit_checkpoints
             ADD COLUMN source_advance_rowid INTEGER NOT NULL DEFAULT 0",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(())
}

async fn validate_existing_audit_checkpoint_baseline(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'table' AND name = 'authority_audit_checkpoints'",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let exists = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| authority_violation("audit checkpoint catalog query returned no row"))?
        .get::<i64>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        != 0;
    drop(rows);
    if !exists {
        return Ok(());
    }

    const REQUIRED_BASELINE_COLUMNS: i64 = 13;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM pragma_table_xinfo('authority_audit_checkpoints')
             WHERE name IN (
                'audit_name', 'audit_version', 'receipt_rowid',
                'observation_sequence', 'provenance_rowid', 'disposition_rowid',
                'alias_rowid', 'projection_checkpoint', 'last_receipts_audited',
                'last_observations_audited', 'last_provenance_audited',
                'last_dispositions_audited', 'last_aliases_audited'
             )",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let found = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| authority_violation("audit checkpoint shape query returned no row"))?
        .get::<i64>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if found != REQUIRED_BASELINE_COLUMNS {
        return Err(authority_violation(
            "authority audit checkpoint table is missing required baseline columns",
        ));
    }
    Ok(())
}

pub(super) async fn read_audit_checkpoint(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<Option<AuditCheckpoint>> {
    let mut rows = conn
        .query(
            "SELECT receipt_rowid, observation_sequence,
                    source_cursor_rowid, source_advance_rowid,
                    provenance_rowid, disposition_rowid, alias_rowid, projection_checkpoint,
                    bounded_passes_since_exhaustive
             FROM authority_audit_checkpoints
             WHERE audit_name = ?1 AND audit_version = ?2",
            params![AUDIT_NAME, AUDIT_VERSION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    else {
        return Ok(None);
    };
    Ok(Some(AuditCheckpoint {
        receipt_rowid: row
            .get(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        observation_sequence: row
            .get(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        source_cursor_rowid: row
            .get(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        source_advance_rowid: row
            .get(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        provenance_rowid: row
            .get(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        disposition_rowid: row
            .get(5)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        alias_rowid: row
            .get(6)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        projection_checkpoint: row
            .get(7)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        bounded_passes_since_exhaustive: row
            .get(8)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
    }))
}

pub(super) async fn audit_checkpoint_is_plausible(
    conn: &impl QueryExecutor,
    checkpoint: AuditCheckpoint,
) -> tracedecay_runtime_core::errors::Result<bool> {
    if checkpoint.receipt_rowid < 0
        || checkpoint.observation_sequence < 0
        || checkpoint.source_cursor_rowid < 0
        || checkpoint.source_advance_rowid < 0
        || checkpoint.provenance_rowid < 0
        || checkpoint.disposition_rowid < 0
        || checkpoint.alias_rowid < 0
        || checkpoint.projection_checkpoint < 0
        || !(-1..MAX_BOUNDED_AUDIT_PASSES).contains(&checkpoint.bounded_passes_since_exhaustive)
    {
        return Ok(false);
    }
    let mut rows = conn
        .query(
            "SELECT
                COALESCE((SELECT MAX(rowid) FROM sanitization_receipts), 0),
                COALESCE((SELECT MAX(sequence) FROM observations), 0),
                COALESCE((SELECT MAX(rowid) FROM source_cursors), 0),
                COALESCE((SELECT MAX(rowid) FROM source_cursor_advances), 0),
                COALESCE((SELECT MAX(rowid) FROM observation_projection_provenance), 0),
                COALESCE((SELECT MAX(rowid) FROM observation_projection_dispositions), 0),
                COALESCE((SELECT MAX(rowid) FROM observation_projection_aliases), 0),
                COALESCE((
                    SELECT last_sequence FROM observation_projection_checkpoints
                    WHERE projector_version = ?1
                ), 0)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| authority_violation("audit checkpoint frontier query returned no row"))?;
    let frontiers = AuditCheckpoint {
        receipt_rowid: row
            .get(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        observation_sequence: row
            .get(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        source_cursor_rowid: row
            .get(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        source_advance_rowid: row
            .get(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        provenance_rowid: row
            .get(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        disposition_rowid: row
            .get(5)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        alias_rowid: row
            .get(6)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        projection_checkpoint: row
            .get(7)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        ..AuditCheckpoint::default()
    };
    Ok(checkpoint.receipt_rowid <= frontiers.receipt_rowid
        && checkpoint.observation_sequence <= frontiers.observation_sequence
        && checkpoint.source_cursor_rowid <= frontiers.source_cursor_rowid
        && checkpoint.source_advance_rowid <= frontiers.source_advance_rowid
        && checkpoint.provenance_rowid <= frontiers.provenance_rowid
        && checkpoint.disposition_rowid <= frontiers.disposition_rowid
        && checkpoint.alias_rowid <= frontiers.alias_rowid
        && checkpoint.projection_checkpoint <= frontiers.projection_checkpoint)
}

#[derive(Clone, Copy)]
struct ProjectionAuthorityState {
    provenance_rows: i64,
    disposition_rows: i64,
    alias_rows: i64,
    workflow_rows: i64,
    queued: bool,
}

impl ProjectionAuthorityState {
    async fn load(
        conn: &impl QueryExecutor,
        observation_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let mut rows = conn
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM observation_projection_provenance
                     WHERE projector_version = ?1 AND observation_id = ?2),
                    (SELECT COUNT(*) FROM observation_projection_dispositions
                     WHERE projector_version = ?1 AND observation_id = ?2),
                    (SELECT COUNT(*) FROM observation_projection_aliases
                     WHERE projector_version = ?1 AND observation_id = ?2),
                    (SELECT COUNT(*) FROM observation_workflow_facts
                     WHERE projector_version = ?1 AND observation_id = ?2),
                    EXISTS(
                        SELECT 1 FROM projection_queue
                        WHERE observation_id = ?2
                    )",
                params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .ok_or_else(|| {
                authority_violation("projection authority count query returned no row")
            })?;
        Ok(Self {
            provenance_rows: row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            disposition_rows: row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            alias_rows: row
                .get(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            workflow_rows: row
                .get(3)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            queued: row
                .get::<i64>(4)
                .map_err(|error| global_db_operation_error(OPERATION, error))?
                != 0,
        })
    }

    fn is_pending_message(self) -> bool {
        self.provenance_rows == 0
            && self.disposition_rows == 0
            && self.workflow_rows == 0
            && self.queued
    }

    fn is_message(self) -> bool {
        self.provenance_rows == 1 && self.disposition_rows == 0
    }

    fn is_pending_skip(self) -> bool {
        self.provenance_rows == 0
            && self.disposition_rows == 0
            && self.alias_rows == 0
            && self.workflow_rows == 0
            && self.queued
    }

    fn is_skip(self) -> bool {
        self.provenance_rows == 0
            && self.disposition_rows == 1
            && self.alias_rows == 0
            && self.workflow_rows == 0
    }
}

struct ProjectionAliasRow {
    provider: String,
    message_id: String,
}

impl ProjectionAliasRow {
    async fn load(
        conn: &impl QueryExecutor,
        observation_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let mut rows = conn
            .query(
                "SELECT output_provider, output_message_id
                 FROM observation_projection_aliases
                 WHERE projector_version = ?1 AND observation_id = ?2",
                params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .ok_or_else(|| authority_violation("projection alias disappeared"))?;
        Ok(Self {
            provider: row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            message_id: row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        })
    }
}

struct ProjectionProvenanceRow {
    retrieval_anchor_id: String,
    receipt_id: String,
    output_provider: String,
    output_message_id: String,
    output_digest: String,
    message_created: i64,
}

impl ProjectionProvenanceRow {
    async fn load(
        conn: &impl QueryExecutor,
        observation_id: &str,
        output_ordinal: i64,
    ) -> tracedecay_runtime_core::errors::Result<Option<Self>> {
        let mut rows = conn
            .query(
                "SELECT retrieval_anchor_id, receipt_id, output_provider, output_message_id,
                        output_digest, message_created
                 FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND observation_id = ?2
                   AND output_ordinal = ?3",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    observation_id,
                    output_ordinal
                ],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            retrieval_anchor_id: row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            receipt_id: row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            output_provider: row
                .get(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            output_message_id: row
                .get(3)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            output_digest: row
                .get(4)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            message_created: row
                .get(5)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        }))
    }
}

struct ProjectionDispositionRow {
    receipt_id: String,
    reason: String,
}

impl ProjectionDispositionRow {
    async fn load(
        conn: &impl QueryExecutor,
        observation_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let mut rows = conn
            .query(
                "SELECT receipt_id, reason FROM observation_projection_dispositions
                 WHERE projector_version = ?1 AND observation_id = ?2",
                params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .ok_or_else(|| authority_violation("projection disposition disappeared"))?;
        Ok(Self {
            receipt_id: row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            reason: row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        })
    }
}

struct ProjectionOutputOwnership {
    creator_count: i64,
}

impl ProjectionOutputOwnership {
    async fn load(
        conn: &impl QueryExecutor,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let mut rows = conn
            .query(
                "SELECT COALESCE(SUM(message_created), 0)
                 FROM observation_projection_provenance
                 WHERE output_provider = ?1 AND output_message_id = ?2",
                params![provider, message_id],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let creator_count = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .ok_or_else(|| authority_violation("projection ownership query returned no row"))?
            .get(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        Ok(Self { creator_count })
    }

    fn validate(self) -> tracedecay_runtime_core::errors::Result<()> {
        if self.creator_count > 1 {
            return Err(authority_violation(
                "projection output has multiple creation owners",
            ));
        }
        Ok(())
    }
}

fn validate_alias_binding(
    alias: &ProjectionAliasRow,
    unaliased: &ObservationProjection,
    projection: &SessionMessageProjection,
) -> tracedecay_runtime_core::errors::Result<()> {
    let unaliased_projection = unaliased.message().ok_or_else(|| {
        authority_violation("projection alias is ineligible without a message output")
    })?;
    let message = projection.message();
    let unaliased_message = unaliased_projection.message();
    let mapped_suffix = format!("/{}", unaliased_message.message_id);
    if alias.provider != message.provider
        || alias.message_id != message.message_id
        || alias.provider != unaliased_message.provider
        || alias.message_id == unaliased_message.message_id
        || !alias.message_id.starts_with("consolidated/")
        || !alias.message_id.ends_with(&mapped_suffix)
    {
        return Err(authority_violation(
            "projection alias is not an eligible consolidation output binding",
        ));
    }
    Ok(())
}

fn validate_provenance_row(
    actual: &ProjectionProvenanceRow,
    projection: &SessionMessageProjection,
) -> tracedecay_runtime_core::errors::Result<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    let output_digest = projection.output_digest().map_err(|_| {
        authority_violation("projection output digest is not canonically derivable")
    })?;
    if actual.retrieval_anchor_id != provenance.retrieval_anchor_id().as_str()
        || actual.receipt_id != provenance.receipt_id()
        || actual.output_provider != message.provider
        || actual.output_message_id != message.message_id
        || actual.output_digest != output_digest.as_str()
        || !matches!(actual.message_created, 0 | 1)
    {
        return Err(authority_violation(
            "projection provenance disagrees with deterministic output",
        ));
    }
    Ok(())
}

async fn validate_message_projection_row(
    conn: &impl QueryExecutor,
    observation_id: &str,
    projection: &SessionMessageProjection,
) -> tracedecay_runtime_core::errors::Result<bool> {
    let message = projection.message();
    let Some(provenance) =
        ProjectionProvenanceRow::load(conn, observation_id, i64::from(projection.output_ordinal()))
            .await?
    else {
        return Ok(false);
    };
    validate_provenance_row(&provenance, projection)?;
    ProjectionOutputOwnership::load(conn, &message.provider, &message.message_id)
        .await?
        .validate()?;
    // Creating an output does not make this observation its current owner: a
    // later generation from the same source supersedes the row while the
    // historical creator keeps `message_created = 1`. Resolve the canonical
    // owner for every observation so a superseded creator is audited against
    // the projection that actually owns the row.
    crate::observation_projection::verify_output_authority(conn, projection)
        .await
        .map_err(|error| {
            authority_violation(format!(
                "projection output rows disagree with deterministic output: {error}"
            ))
        })?;
    Ok(true)
}

async fn validate_message_projection(
    conn: &impl QueryExecutor,
    observation_id: &str,
    state: ProjectionAuthorityState,
    unaliased: &ObservationProjection,
    projection: &SessionMessageProjection,
) -> tracedecay_runtime_core::errors::Result<()> {
    if state.alias_rows > 1 {
        return Err(authority_violation(
            "projection authority must contain exactly one message outcome",
        ));
    }
    if state.alias_rows == 1 {
        let alias = ProjectionAliasRow::load(conn, observation_id).await?;
        validate_alias_binding(&alias, unaliased, projection)?;
    }
    if state.is_pending_message() {
        return Ok(());
    }
    if !state.is_message() {
        return Err(authority_violation(
            "projection authority must contain exactly one message outcome",
        ));
    }
    if !validate_message_projection_row(conn, observation_id, projection).await? {
        return Err(authority_violation("projection provenance disappeared"));
    }
    Ok(())
}

async fn validate_skipped_projection(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    state: ProjectionAuthorityState,
    reason: ProjectionSkipReason,
) -> tracedecay_runtime_core::errors::Result<()> {
    let observation_id = observation.observation_id().as_str();
    if state.is_pending_skip() {
        return Ok(());
    }
    if !state.is_skip() {
        return Err(authority_violation(
            "projection authority must contain exactly one skip outcome without an alias",
        ));
    }
    let disposition = ProjectionDispositionRow::load(conn, observation_id).await?;
    validate_skipped_projection_row(observation, &disposition, reason)
}

fn validate_skipped_projection_row(
    observation: &DurableObservationV1,
    disposition: &ProjectionDispositionRow,
    reason: ProjectionSkipReason,
) -> tracedecay_runtime_core::errors::Result<()> {
    if disposition.receipt_id != observation.receipt().receipt().receipt_id().as_str()
        || disposition.reason != reason.as_str()
    {
        return Err(authority_violation(
            "projection disposition disagrees with deterministic skip reason",
        ));
    }
    Ok(())
}

async fn validate_composite_projection(
    conn: &impl QueryExecutor,
    observation_id: &str,
    state: ProjectionAuthorityState,
    unaliased: &ObservationProjection,
    message: Option<&SessionMessageProjection>,
    derived_messages: &[SessionMessageProjection],
    workflow_facts: &[WorkflowFactProjection],
) -> tracedecay_runtime_core::errors::Result<()> {
    if workflow_facts.is_empty() && derived_messages.is_empty() {
        return Err(authority_violation(
            "composite projection has no additional output",
        ));
    }
    if state.alias_rows > 1 || (message.is_none() && state.alias_rows != 0) {
        return Err(authority_violation(
            "composite projection has invalid alias authority",
        ));
    }
    if state.alias_rows == 1 {
        let projection = message.ok_or_else(|| {
            authority_violation("projection alias is ineligible without a message output")
        })?;
        let alias = ProjectionAliasRow::load(conn, observation_id).await?;
        validate_alias_binding(&alias, unaliased, projection)?;
    }
    let expected_message_rows =
        i64::try_from(usize::from(message.is_some()) + derived_messages.len())
            .map_err(|_| authority_violation("message projection count overflow"))?;
    if state.queued {
        if state.disposition_rows != 0
            || state.workflow_rows != 0
            || state.provenance_rows > expected_message_rows
        {
            return Err(authority_violation(
                "pending composite projection contains invalid partial output",
            ));
        }
        let mut validated_rows = 0_i64;
        if let Some(projection) = message {
            validated_rows +=
                i64::from(validate_message_projection_row(conn, observation_id, projection).await?);
        }
        for projection in derived_messages {
            validated_rows +=
                i64::from(validate_message_projection_row(conn, observation_id, projection).await?);
        }
        if validated_rows != state.provenance_rows {
            return Err(authority_violation(
                "pending composite projection contains unexpected message authority",
            ));
        }
        return Ok(());
    }
    if state.disposition_rows != 0 || state.provenance_rows != expected_message_rows {
        return Err(authority_violation(
            "composite projection has incomplete message authority",
        ));
    }
    if let Some(projection) = message
        && !validate_message_projection_row(conn, observation_id, projection).await?
    {
        return Err(authority_violation("projection provenance disappeared"));
    }
    for projection in derived_messages {
        if !validate_message_projection_row(conn, observation_id, projection).await? {
            return Err(authority_violation("projection provenance disappeared"));
        }
    }
    let expected_workflow_rows = i64::try_from(workflow_facts.len())
        .map_err(|_| authority_violation("workflow projection count overflow"))?;
    if state.workflow_rows != expected_workflow_rows {
        return Err(authority_violation(
            "workflow projection authority has incomplete output",
        ));
    }
    crate::observation_projection::verify_workflow_effects(conn, workflow_facts)
        .await
        .map_err(|error| {
            authority_violation(format!(
                "workflow projection rows disagree with deterministic output: {error}"
            ))
        })
}

async fn validate_projection_effect(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
) -> tracedecay_runtime_core::errors::Result<()> {
    // Derivation is disposition-aware, so an observation that converged to a
    // durable output-collision skip re-derives as `Skipped(OutputCollision)`
    // and audits through the `Skipped` arm below natively. The unaliased
    // projection is only needed to validate alias bindings, so it is derived
    // lazily inside the arms that consume it.
    let effect = crate::observation_projection::derive_projection_with_alias(conn, observation)
        .await
        .map_err(|error| authority_violation(format!("invalid projection authority: {error}")))?;
    let observation_id = observation.observation_id().as_str();
    let state = ProjectionAuthorityState::load(conn, observation_id).await?;
    match &effect {
        ObservationProjection::Message(projection) => {
            if state.workflow_rows != 0 {
                return Err(authority_violation(
                    "message projection contains unexpected workflow output",
                ));
            }
            let unaliased = derive_unaliased_projection(observation)?;
            validate_message_projection(conn, observation_id, state, &unaliased, projection).await
        }
        ObservationProjection::Composite {
            message,
            derived_messages,
            workflow_facts,
        } => {
            let unaliased = derive_unaliased_projection(observation)?;
            validate_composite_projection(
                conn,
                observation_id,
                state,
                &unaliased,
                message.as_deref(),
                derived_messages,
                workflow_facts,
            )
            .await
        }
        ObservationProjection::Skipped(reason) => {
            validate_skipped_projection(conn, observation, state, *reason).await
        }
    }
}

fn derive_unaliased_projection(
    observation: &DurableObservationV1,
) -> tracedecay_runtime_core::errors::Result<ObservationProjection> {
    crate::observation_projection::derive_projection(observation)
        .map_err(|error| authority_violation(format!("invalid projection authority: {error}")))
}

async fn observation_by_id(
    conn: &impl QueryExecutor,
    observation_id: &str,
) -> tracedecay_runtime_core::errors::Result<DurableObservationV1> {
    let mut rows = conn
        .query(
            "SELECT observation_json FROM observations WHERE observation_id = ?1",
            params![observation_id],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let json = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| {
            authority_violation("projection authority references a missing observation")
        })?
        .get::<String>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    decode_authority_json(&json, "projected observation authority JSON")
}

async fn count_suffix_rows(
    conn: &impl QueryExecutor,
    table: &str,
    after_rowid: i64,
) -> tracedecay_runtime_core::errors::Result<(i64, i64)> {
    let query = format!(
        "SELECT COALESCE(MAX(rowid), ?1), COUNT(*) FROM {table}
         WHERE rowid > ?1 AND projector_version = ?2"
    );
    let mut rows = conn
        .query(
            &query,
            params![after_rowid, SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| authority_violation("projection suffix count returned no row"))?;
    Ok((
        row.get(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        row.get(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
    ))
}

/// Pages one projection-authority table's rowid suffix, collecting the
/// observation ids it references.
async fn collect_projection_suffix_ids(
    conn: &impl QueryExecutor,
    table: &str,
    after_rowid: i64,
    through_observation_sequence: i64,
    observation_ids: &mut BTreeSet<String>,
) -> tracedecay_runtime_core::errors::Result<()> {
    let query = format!(
        "SELECT projection.rowid, projection.observation_id
         FROM {table} AS projection
         JOIN observations AS observation
           ON observation.observation_id = projection.observation_id
         WHERE projection.rowid > ?1
           AND projection.projector_version = ?2
           AND observation.sequence <= ?3
         ORDER BY projection.rowid LIMIT ?4"
    );
    let mut scan_cursor = after_rowid;
    loop {
        let mut rows = conn
            .query(
                &query,
                params![
                    scan_cursor,
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    through_observation_sequence,
                    AUDIT_PAGE_ROWS
                ],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            page_rows += 1;
            scan_cursor = row
                .get::<i64>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            observation_ids.insert(
                row.get::<String>(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            );
        }
        drop(rows);
        if page_rows < AUDIT_PAGE_ROWS {
            return Ok(());
        }
    }
}

async fn projection_rowid_through_sequence(
    conn: &impl QueryExecutor,
    table: &str,
    through_observation_sequence: i64,
) -> tracedecay_runtime_core::errors::Result<i64> {
    let query = format!(
        "SELECT COALESCE(MAX(projection.rowid), 0)
         FROM {table} AS projection
         JOIN observations AS observation
           ON observation.observation_id = projection.observation_id
         WHERE projection.projector_version = ?1
           AND observation.sequence <= ?2"
    );
    let mut rows = conn
        .query(
            &query,
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                through_observation_sequence
            ],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| authority_violation("projection progress query returned no row"))?
        .get(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

async fn projection_audit_checkpoint_through_sequence(
    conn: &impl QueryExecutor,
    checkpoint: AuditCheckpoint,
    observation_sequence: i64,
) -> tracedecay_runtime_core::errors::Result<AuditCheckpoint> {
    if checkpoint.bounded_passes_since_exhaustive == INCOMPLETE_EXHAUSTIVE_PASS {
        return Ok(AuditCheckpoint {
            projection_checkpoint: observation_sequence,
            ..checkpoint
        });
    }
    Ok(AuditCheckpoint {
        provenance_rowid: projection_rowid_through_sequence(
            conn,
            "observation_projection_provenance",
            observation_sequence,
        )
        .await?,
        disposition_rowid: projection_rowid_through_sequence(
            conn,
            "observation_projection_dispositions",
            observation_sequence,
        )
        .await?,
        alias_rowid: projection_rowid_through_sequence(
            conn,
            "observation_projection_aliases",
            observation_sequence,
        )
        .await?,
        projection_checkpoint: observation_sequence,
        ..checkpoint
    })
}

fn historical_projection_delta_required(checkpoint: AuditCheckpoint) -> bool {
    checkpoint.bounded_passes_since_exhaustive != INCOMPLETE_EXHAUSTIVE_PASS
}

async fn validate_projection_authority_suffix_pages(
    conn: &impl QueryExecutor,
    mut checkpoint: AuditCheckpoint,
    page_limit: Option<i64>,
) -> tracedecay_runtime_core::errors::Result<(AuditCheckpoint, i64, i64, i64, bool)> {
    let (provenance_rowid, provenance_audited) = count_suffix_rows(
        conn,
        "observation_projection_provenance",
        checkpoint.provenance_rowid,
    )
    .await?;
    let (disposition_rowid, dispositions_audited) = count_suffix_rows(
        conn,
        "observation_projection_dispositions",
        checkpoint.disposition_rowid,
    )
    .await?;
    let (alias_rowid, aliases_audited) = count_suffix_rows(
        conn,
        "observation_projection_aliases",
        checkpoint.alias_rowid,
    )
    .await?;
    let current_projection_checkpoint = projection_checkpoint(conn).await?;
    let coverage_start = if current_projection_checkpoint < checkpoint.projection_checkpoint {
        0
    } else {
        checkpoint.projection_checkpoint
    };
    // Audit the frontier suffix page-wise. Most historical observations are
    // durable skips; loading their observation, state and disposition with
    // separate SQL-channel requests made the exhaustive pass issue five
    // round-trips per row. The page query carries that state together so a
    // common skip is validated without any per-row database request.
    let mut scan_cursor = coverage_start;
    let mut pages_audited = 0_i64;
    loop {
        let mut rows = conn
            .query(
                "SELECT observation.sequence, observation.observation_json,
                        observation.receipt_id,
                        (SELECT COUNT(*) FROM observation_projection_provenance
                         WHERE projector_version = ?1
                           AND observation_id = observation.observation_id),
                        (SELECT COUNT(*) FROM observation_projection_dispositions
                         WHERE projector_version = ?1
                           AND observation_id = observation.observation_id),
                        (SELECT COUNT(*) FROM observation_projection_aliases
                         WHERE projector_version = ?1
                           AND observation_id = observation.observation_id),
                        (SELECT COUNT(*) FROM observation_workflow_facts
                         WHERE projector_version = ?1
                           AND observation_id = observation.observation_id),
                        EXISTS(
                            SELECT 1 FROM projection_queue
                            WHERE observation_id = observation.observation_id
                        ),
                        disposition.receipt_id, disposition.reason
                 FROM observations AS observation
                 LEFT JOIN observation_projection_dispositions AS disposition
                   ON disposition.projector_version = ?1
                  AND disposition.observation_id = observation.observation_id
                 WHERE observation.sequence > ?2 AND observation.sequence <= ?3
                 ORDER BY observation.sequence LIMIT ?4",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    scan_cursor,
                    current_projection_checkpoint,
                    AUDIT_PAGE_ROWS
                ],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut page_rows = 0_i64;
        let mut detailed_observations = Vec::<(i64, DurableObservationV1)>::new();
        let mut detailed_limit_reached = false;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            page_rows += 1;
            scan_cursor = row
                .get::<i64>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let observation_receipt_id = row
                .get::<String>(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let state = ProjectionAuthorityState {
                provenance_rows: row
                    .get(3)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                disposition_rows: row
                    .get(4)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                alias_rows: row
                    .get(5)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                workflow_rows: row
                    .get(6)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                queued: row
                    .get::<i64>(7)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?
                    != 0,
            };
            let disposition = match (
                row.get::<Option<String>>(8)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                row.get::<Option<String>>(9)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            ) {
                (Some(receipt_id), Some(reason)) => {
                    Some(ProjectionDispositionRow { receipt_id, reason })
                }
                (None, None) => None,
                _ => {
                    return Err(authority_violation(
                        "projection disposition contains incomplete authority",
                    ));
                }
            };
            let stored_collision = disposition.as_ref().is_some_and(|disposition| {
                disposition.reason == ProjectionSkipReason::OutputCollision.as_str()
            });
            if stored_collision {
                if !state.is_skip() {
                    return Err(authority_violation(
                        "projection authority must contain exactly one collision skip outcome",
                    ));
                }
                let disposition = disposition
                    .as_ref()
                    .ok_or_else(|| authority_violation("projection disposition disappeared"))?;
                if disposition.receipt_id != observation_receipt_id {
                    return Err(authority_violation(
                        "projection collision disposition disagrees with observation receipt",
                    ));
                }
                continue;
            }
            let observation = decode_authority_json::<DurableObservationV1>(
                &row.get::<String>(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                "projected observation authority JSON",
            )?;
            let skip_reason = match crate::observation_projection::derive_projection(&observation)
                .map_err(|error| {
                authority_violation(format!("invalid projection authority: {error}"))
            })? {
                ObservationProjection::Skipped(reason) => Some(reason),
                ObservationProjection::Message(_) | ObservationProjection::Composite { .. } => None,
            };
            if let Some(reason) = skip_reason {
                if !state.is_skip() {
                    return Err(authority_violation(
                        "projection authority must contain exactly one skip outcome without an alias",
                    ));
                }
                let disposition = disposition
                    .as_ref()
                    .ok_or_else(|| authority_violation("projection disposition disappeared"))?;
                validate_skipped_projection_row(&observation, disposition, reason)?;
            } else {
                detailed_observations.push((scan_cursor, observation));
                if detailed_observations.len() >= MAX_DETAILED_OBSERVATIONS_PER_PAGE {
                    detailed_limit_reached = true;
                    break;
                }
            }
        }
        drop(rows);
        let validation_concurrency = if detailed_limit_reached {
            DETAILED_AUDIT_CONCURRENCY
        } else {
            // The final partial page can contain unusually expensive composite
            // outputs. Checkpoint smaller chunks so interruption never restarts
            // the whole tail.
            DETAILED_TAIL_CONCURRENCY
        };
        for chunk in detailed_observations.chunks(validation_concurrency) {
            try_join_all(
                chunk
                    .iter()
                    .map(|(_, observation)| validate_projection_effect(conn, observation)),
            )
            .await?;
            let validated_through = chunk.last().map_or(scan_cursor, |(sequence, _)| *sequence);
            checkpoint =
                projection_audit_checkpoint_through_sequence(conn, checkpoint, validated_through)
                    .await?;
        }
        pages_audited += 1;
        if page_rows < AUDIT_PAGE_ROWS && !detailed_limit_reached {
            break;
        }
        if page_limit.is_some_and(|limit| pages_audited >= limit) {
            checkpoint =
                projection_audit_checkpoint_through_sequence(conn, checkpoint, scan_cursor).await?;
            return Ok((
                checkpoint,
                provenance_audited,
                dispositions_audited,
                aliases_audited,
                false,
            ));
        }
    }
    // Projection rows added for an already-audited observation have rowids
    // beyond the table checkpoints even though their observation sequence is
    // at or below `coverage_start`. Validate only that historical delta; the
    // frontier suffix above already covered every newer observation.
    if historical_projection_delta_required(checkpoint) {
        let mut observation_ids = BTreeSet::new();
        collect_projection_suffix_ids(
            conn,
            "observation_projection_provenance",
            checkpoint.provenance_rowid,
            coverage_start,
            &mut observation_ids,
        )
        .await?;
        collect_projection_suffix_ids(
            conn,
            "observation_projection_dispositions",
            checkpoint.disposition_rowid,
            coverage_start,
            &mut observation_ids,
        )
        .await?;
        collect_projection_suffix_ids(
            conn,
            "observation_projection_aliases",
            checkpoint.alias_rowid,
            coverage_start,
            &mut observation_ids,
        )
        .await?;
        let mut historical_observations = Vec::with_capacity(observation_ids.len());
        for observation_id in observation_ids {
            let observation = observation_by_id(conn, &observation_id).await?;
            historical_observations.push(observation);
        }
        for chunk in historical_observations.chunks(DETAILED_AUDIT_CONCURRENCY) {
            try_join_all(
                chunk
                    .iter()
                    .map(|observation| validate_projection_effect(conn, observation)),
            )
            .await?;
        }
    }
    Ok((
        AuditCheckpoint {
            provenance_rowid,
            disposition_rowid,
            alias_rowid,
            projection_checkpoint: current_projection_checkpoint,
            ..checkpoint
        },
        provenance_audited,
        dispositions_audited,
        aliases_audited,
        true,
    ))
}

pub(super) async fn validate_projection_authority_suffix(
    conn: &impl QueryExecutor,
    checkpoint: AuditCheckpoint,
) -> tracedecay_runtime_core::errors::Result<(AuditCheckpoint, i64, i64, i64)> {
    let (checkpoint, provenance, dispositions, aliases, _) =
        validate_projection_authority_suffix_pages(conn, checkpoint, None).await?;
    Ok((checkpoint, provenance, dispositions, aliases))
}

pub(super) async fn validate_projection_authority_chunk(
    conn: &impl QueryExecutor,
    checkpoint: AuditCheckpoint,
) -> tracedecay_runtime_core::errors::Result<(AuditCheckpoint, i64, i64, i64, bool)> {
    validate_projection_authority_suffix_pages(
        conn,
        checkpoint,
        Some(PROJECTION_PROGRESS_PAGE_INTERVAL),
    )
    .await
}

pub(super) async fn write_audit_checkpoint(
    conn: &impl Executor,
    progress: AuditProgress,
) -> tracedecay_runtime_core::errors::Result<()> {
    let checkpoint = progress.checkpoint;
    conn.execute(
        "INSERT INTO authority_audit_checkpoints (
            audit_name, audit_version, receipt_rowid, observation_sequence,
            source_cursor_rowid, source_advance_rowid,
            provenance_rowid, disposition_rowid, alias_rowid, projection_checkpoint,
            last_receipts_audited, last_observations_audited,
            last_provenance_audited, last_dispositions_audited, last_aliases_audited,
            bounded_passes_since_exhaustive
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
         )
         ON CONFLICT(audit_name) DO UPDATE SET
            audit_version = excluded.audit_version,
            receipt_rowid = excluded.receipt_rowid,
            observation_sequence = excluded.observation_sequence,
            source_cursor_rowid = excluded.source_cursor_rowid,
            source_advance_rowid = excluded.source_advance_rowid,
            provenance_rowid = excluded.provenance_rowid,
            disposition_rowid = excluded.disposition_rowid,
            alias_rowid = excluded.alias_rowid,
            projection_checkpoint = excluded.projection_checkpoint,
            last_receipts_audited = excluded.last_receipts_audited,
            last_observations_audited = excluded.last_observations_audited,
            last_provenance_audited = excluded.last_provenance_audited,
            last_dispositions_audited = excluded.last_dispositions_audited,
            last_aliases_audited = excluded.last_aliases_audited,
            bounded_passes_since_exhaustive = excluded.bounded_passes_since_exhaustive",
        params![
            AUDIT_NAME,
            AUDIT_VERSION,
            checkpoint.receipt_rowid,
            checkpoint.observation_sequence,
            checkpoint.source_cursor_rowid,
            checkpoint.source_advance_rowid,
            checkpoint.provenance_rowid,
            checkpoint.disposition_rowid,
            checkpoint.alias_rowid,
            checkpoint.projection_checkpoint,
            progress.receipts_audited,
            progress.observations_audited,
            progress.provenance_audited,
            progress.dispositions_audited,
            progress.aliases_audited,
            checkpoint.bounded_passes_since_exhaustive
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OPERATION, error))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::Value;
    use tempfile::TempDir;
    use tracedecay_domain::{
        CanonicalObservationEnvelopeV1, ComponentVersion, DurableObservationV1, ObservationId,
        ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
        SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    };
    use tracedecay_store::{ObservationProjection, SESSION_MESSAGE_PROJECTOR_VERSION};

    use super::{
        AuditCheckpoint, DETAILED_AUDIT_CHUNKS_PER_PAGE, DETAILED_AUDIT_CONCURRENCY,
        DETAILED_TAIL_CONCURRENCY, MAX_DETAILED_OBSERVATIONS_PER_PAGE,
        PROJECTION_PROGRESS_PAGE_INTERVAL, ensure_audit_checkpoint_schema,
        historical_projection_delta_required, projection_audit_checkpoint_through_sequence,
        validate_projection_authority_suffix,
    };
    use crate::ensure_registered_schema;
    use tracedecay_runtime_core::db::engine::{
        Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows, TestConnection, params,
    };

    struct CountingQuery<'a> {
        inner: &'a TestConnection,
        queries: AtomicUsize,
    }

    impl QueryExecutor for CountingQuery<'_> {
        async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
        where
            P: IntoParams,
        {
            self.queries.fetch_add(1, Ordering::Relaxed);
            self.inner.query(sql, params).await
        }
    }

    impl Executor for CountingQuery<'_> {
        async fn execute<P>(&self, sql: &str, params: P) -> EngineResult<u64>
        where
            P: IntoParams,
        {
            self.inner.execute(sql, params).await
        }

        async fn execute_batch(&self, sql: &str) -> EngineResult<()> {
            self.inner.execute_batch(sql).await
        }
    }

    #[test]
    fn exhaustive_projection_audit_bounds_detailed_work() {
        assert_eq!(
            MAX_DETAILED_OBSERVATIONS_PER_PAGE,
            DETAILED_AUDIT_CONCURRENCY * DETAILED_AUDIT_CHUNKS_PER_PAGE
        );
        assert!(std::hint::black_box(DETAILED_TAIL_CONCURRENCY) < DETAILED_AUDIT_CONCURRENCY);
        assert_eq!(PROJECTION_PROGRESS_PAGE_INTERVAL, 1);
    }

    #[tokio::test]
    async fn audit_checkpoint_schema_tracks_source_cursor_progress() {
        let directory = TempDir::new().unwrap();
        let connection = TestConnection::open(&directory.path().join("sessions.db"));
        connection
            .execute_batch(
                "CREATE TABLE authority_audit_checkpoints (
                    audit_name TEXT PRIMARY KEY,
                    audit_version INTEGER NOT NULL,
                    receipt_rowid INTEGER NOT NULL,
                    observation_sequence INTEGER NOT NULL,
                    provenance_rowid INTEGER NOT NULL,
                    disposition_rowid INTEGER NOT NULL,
                    alias_rowid INTEGER NOT NULL,
                    projection_checkpoint INTEGER NOT NULL,
                    last_receipts_audited INTEGER NOT NULL,
                    last_observations_audited INTEGER NOT NULL,
                    last_provenance_audited INTEGER NOT NULL,
                    last_dispositions_audited INTEGER NOT NULL,
                    last_aliases_audited INTEGER NOT NULL,
                    bounded_passes_since_exhaustive INTEGER NOT NULL DEFAULT 0
                );",
            )
            .await
            .unwrap();
        ensure_audit_checkpoint_schema(&connection).await.unwrap();

        let mut rows = connection
            .query(
                "SELECT name FROM pragma_table_xinfo('authority_audit_checkpoints')
                 WHERE name IN ('source_cursor_rowid', 'source_advance_rowid')
                 ORDER BY name",
                (),
            )
            .await
            .unwrap();
        let mut columns = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            columns.push(row.get::<String>(0).unwrap());
        }

        assert_eq!(
            columns,
            ["source_advance_rowid", "source_cursor_rowid"],
            "source authority scans need durable seek positions"
        );
    }

    #[tokio::test]
    async fn invariant_receipt_probes_have_covering_indexes() {
        let directory = TempDir::new().unwrap();
        let connection = TestConnection::open(&directory.path().join("sessions.db"));
        ensure_registered_schema(&connection).await.unwrap();

        let mut rows = connection
            .query(
                "SELECT name FROM sqlite_master
                 WHERE type = 'index'
                   AND name IN (
                       'idx_observations_identity_receipt',
                       'idx_projection_dispositions_observation_receipt'
                   )
                 ORDER BY name",
                (),
            )
            .await
            .unwrap();
        let mut indexes = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            indexes.push(row.get::<String>(0).unwrap());
        }

        assert_eq!(
            indexes,
            [
                "idx_observations_identity_receipt",
                "idx_projection_dispositions_observation_receipt"
            ]
        );
    }

    #[test]
    fn incomplete_exhaustive_pass_does_not_repeat_historical_projection_audit() {
        assert!(!historical_projection_delta_required(AuditCheckpoint {
            bounded_passes_since_exhaustive: -1,
            ..AuditCheckpoint::default()
        }));
        assert!(historical_projection_delta_required(
            AuditCheckpoint::default()
        ));
    }

    #[tokio::test]
    async fn incomplete_exhaustive_checkpoint_does_not_rescan_projection_tables() {
        let directory = TempDir::new().unwrap();
        let connection = TestConnection::open(&directory.path().join("sessions.db"));
        ensure_registered_schema(&connection).await.unwrap();
        let counting = CountingQuery {
            inner: &connection,
            queries: AtomicUsize::new(0),
        };
        let checkpoint = AuditCheckpoint {
            provenance_rowid: 11,
            disposition_rowid: 22,
            alias_rowid: 33,
            bounded_passes_since_exhaustive: -1,
            ..AuditCheckpoint::default()
        };

        let checkpoint = projection_audit_checkpoint_through_sequence(&counting, checkpoint, 44)
            .await
            .unwrap();

        assert_eq!(checkpoint.provenance_rowid, 11);
        assert_eq!(checkpoint.disposition_rowid, 22);
        assert_eq!(checkpoint.alias_rowid, 33);
        assert_eq!(checkpoint.projection_checkpoint, 44);
        assert_eq!(counting.queries.load(Ordering::Relaxed), 0);
    }

    fn skipped_observation(index: usize) -> DurableObservationV1 {
        let record_id = format!("record.audit-page-{index}");
        let session_id = format!("audit-page-{index}");
        let mut fixture: Value = serde_json::from_str(include_str!(
            "../../../../../tests/fixtures/provider_normalization/codex/session_meta.expected_envelope.json"
        ))
        .unwrap();
        fixture["stable_record_id"] = Value::String(record_id.clone());
        fixture["relations"]["session_id"] = Value::String(session_id.clone());
        fixture["relations"]["thread_id"] = Value::String(session_id);
        let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_value(fixture).unwrap();
        let source = ObservationSourceIdentityV1::for_provider(
            envelope.provider().clone(),
            envelope.relations().session_id().clone(),
        )
        .unwrap();
        let payload = serde_json::to_value(envelope).unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(format!("receipt.audit-page-{index}")).unwrap(),
                ComponentVersion::new("sanitizer.audit-page.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        DurableObservationV1::new(
            ObservationIdentityMaterialV1::for_native_record(
                source,
                ObservationScopeV1::Profile,
                ObservationSourceGenerationV1::new(1).unwrap(),
                ObservationSourceRangeV1::new(0, 1).unwrap(),
                ObservationOrderingDomainV1::FileBytes,
                ObservationId::new(record_id).unwrap(),
            )
            .unwrap(),
            receipt,
            RetentionClass::new("retention.audit-page").unwrap(),
            payload,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn projection_audit_batches_common_skip_rows() {
        const OBSERVATIONS: usize = 128;

        let directory = TempDir::new().unwrap();
        let connection = TestConnection::open(&directory.path().join("sessions.db"));
        ensure_registered_schema(&connection).await.unwrap();
        for index in 0..OBSERVATIONS {
            let observation = skipped_observation(index);
            let receipt = observation.receipt();
            let effect = crate::observation_projection::derive_projection(&observation).unwrap();
            let ObservationProjection::Skipped(reason) = effect else {
                panic!("session metadata fixture must project as a skip");
            };
            connection
                .execute(
                    "INSERT INTO sanitization_receipts (
                        receipt_id, sanitizer_version, payload_digest, receipt_json
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        receipt.receipt().receipt_id().as_str(),
                        receipt.receipt().sanitizer_version().as_str(),
                        observation.payload_reference().digest().as_str(),
                        serde_json::to_string(receipt).unwrap()
                    ],
                )
                .await
                .unwrap();
            connection
                .execute(
                    "INSERT INTO observations (
                        observation_id, payload_digest, receipt_id,
                        observation_json, committed_cursor_json
                     ) VALUES (?1, ?2, ?3, ?4, '{}')",
                    params![
                        observation.observation_id().as_str(),
                        observation.payload_reference().digest().as_str(),
                        receipt.receipt().receipt_id().as_str(),
                        serde_json::to_string(&observation).unwrap()
                    ],
                )
                .await
                .unwrap();
            connection
                .execute(
                    "INSERT INTO observation_projection_dispositions (
                        projector_version, observation_id, receipt_id, reason
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        SESSION_MESSAGE_PROJECTOR_VERSION,
                        observation.observation_id().as_str(),
                        receipt.receipt().receipt_id().as_str(),
                        reason.as_str()
                    ],
                )
                .await
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO observation_projection_checkpoints (
                    projector_version, last_sequence
                 ) VALUES (?1, ?2)",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    i64::try_from(OBSERVATIONS).unwrap()
                ],
            )
            .await
            .unwrap();

        let counting = CountingQuery {
            inner: &connection,
            queries: AtomicUsize::new(0),
        };
        let (_, _, dispositions, _) =
            validate_projection_authority_suffix(&counting, AuditCheckpoint::default())
                .await
                .unwrap();
        assert_eq!(dispositions, i64::try_from(OBSERVATIONS).unwrap());
        assert!(
            counting.queries.load(Ordering::Relaxed) < OBSERVATIONS / 2,
            "projection audit issued {} queries for {OBSERVATIONS} common skip rows",
            counting.queries.load(Ordering::Relaxed)
        );
    }
}

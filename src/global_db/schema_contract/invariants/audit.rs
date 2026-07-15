use std::collections::BTreeSet;

use libsql::{Connection, params};
use tracedecay_domain::DurableObservationV1;
use tracedecay_store::{
    ObservationProjection, ProjectionSkipReason, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SessionMessageProjection,
};

use crate::global_db::global_db_operation_error;

use super::rows::{authority_violation, decode_authority_json};
use super::{OPERATION, projection_checkpoint};
const AUDIT_NAME: &str = "observation-authority";

const AUDIT_VERSION: i64 = 2;
pub(super) const MAX_BOUNDED_AUDIT_PASSES: i64 = 64;

#[derive(Clone, Copy, Default)]
pub(super) struct AuditCheckpoint {
    pub(super) receipt_rowid: i64,
    pub(super) observation_sequence: i64,
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

pub(super) async fn ensure_audit_checkpoint_schema(conn: &Connection) -> crate::errors::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS authority_audit_checkpoints (
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
    Ok(())
}

pub(super) async fn read_audit_checkpoint(
    conn: &Connection,
) -> crate::errors::Result<Option<AuditCheckpoint>> {
    let mut rows = conn
        .query(
            "SELECT receipt_rowid, observation_sequence, provenance_rowid,
                    disposition_rowid, alias_rowid, projection_checkpoint,
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
        provenance_rowid: row
            .get(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        disposition_rowid: row
            .get(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        alias_rowid: row
            .get(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        projection_checkpoint: row
            .get(5)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        bounded_passes_since_exhaustive: row
            .get(6)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
    }))
}

pub(super) async fn audit_checkpoint_is_plausible(
    conn: &Connection,
    checkpoint: AuditCheckpoint,
) -> crate::errors::Result<bool> {
    if checkpoint.receipt_rowid < 0
        || checkpoint.observation_sequence < 0
        || checkpoint.provenance_rowid < 0
        || checkpoint.disposition_rowid < 0
        || checkpoint.alias_rowid < 0
        || checkpoint.projection_checkpoint < 0
        || !(0..MAX_BOUNDED_AUDIT_PASSES).contains(&checkpoint.bounded_passes_since_exhaustive)
    {
        return Ok(false);
    }
    let mut rows = conn
        .query(
            "SELECT
                COALESCE((SELECT MAX(rowid) FROM sanitization_receipts), 0),
                COALESCE((SELECT MAX(sequence) FROM observations), 0),
                COALESCE((SELECT MAX(rowid) FROM observation_projection_provenance), 0),
                COALESCE((SELECT MAX(rowid) FROM observation_projection_dispositions), 0),
                COALESCE((SELECT MAX(rowid) FROM observation_projection_aliases), 0),
                COALESCE((
                    SELECT last_sequence FROM observation_projection_checkpoints
                    WHERE projector_version = ?1
                ), 0)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V1],
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
        provenance_rowid: row
            .get(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        disposition_rowid: row
            .get(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        alias_rowid: row
            .get(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        projection_checkpoint: row
            .get(5)
            .map_err(|error| global_db_operation_error(OPERATION, error))?,
        ..AuditCheckpoint::default()
    };
    Ok(checkpoint.receipt_rowid <= frontiers.receipt_rowid
        && checkpoint.observation_sequence <= frontiers.observation_sequence
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
    queued: bool,
}

impl ProjectionAuthorityState {
    async fn load(conn: &Connection, observation_id: &str) -> crate::errors::Result<Self> {
        let mut rows = conn
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM observation_projection_provenance
                     WHERE projector_version = ?1 AND observation_id = ?2),
                    (SELECT COUNT(*) FROM observation_projection_dispositions
                     WHERE projector_version = ?1 AND observation_id = ?2),
                    (SELECT COUNT(*) FROM observation_projection_aliases
                     WHERE projector_version = ?1 AND observation_id = ?2),
                    EXISTS(
                        SELECT 1 FROM projection_queue
                        WHERE observation_id = ?2
                    )",
                params![SESSION_MESSAGE_PROJECTOR_VERSION_V1, observation_id],
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
            queued: row
                .get::<i64>(3)
                .map_err(|error| global_db_operation_error(OPERATION, error))?
                != 0,
        })
    }

    fn is_pending_message(self) -> bool {
        self.provenance_rows == 0 && self.disposition_rows == 0 && self.queued
    }

    fn is_message(self) -> bool {
        self.provenance_rows == 1 && self.disposition_rows == 0
    }

    fn is_pending_skip(self) -> bool {
        self.provenance_rows == 0
            && self.disposition_rows == 0
            && self.alias_rows == 0
            && self.queued
    }

    fn is_skip(self) -> bool {
        self.provenance_rows == 0 && self.disposition_rows == 1 && self.alias_rows == 0
    }
}

struct ProjectionAliasRow {
    provider: String,
    message_id: String,
}

impl ProjectionAliasRow {
    async fn load(conn: &Connection, observation_id: &str) -> crate::errors::Result<Self> {
        let mut rows = conn
            .query(
                "SELECT output_provider, output_message_id
                 FROM observation_projection_aliases
                 WHERE projector_version = ?1 AND observation_id = ?2",
                params![SESSION_MESSAGE_PROJECTOR_VERSION_V1, observation_id],
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
    receipt_id: String,
    output_provider: String,
    output_message_id: String,
    output_digest: String,
    message_created: i64,
}

impl ProjectionProvenanceRow {
    async fn load(conn: &Connection, observation_id: &str) -> crate::errors::Result<Self> {
        let mut rows = conn
            .query(
                "SELECT receipt_id, output_provider, output_message_id,
                        output_digest, message_created
                 FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND observation_id = ?2",
                params![SESSION_MESSAGE_PROJECTOR_VERSION_V1, observation_id],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .ok_or_else(|| authority_violation("projection provenance disappeared"))?;
        Ok(Self {
            receipt_id: row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            output_provider: row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            output_message_id: row
                .get(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            output_digest: row
                .get(3)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            message_created: row
                .get(4)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        })
    }
}

struct ProjectionDispositionRow {
    receipt_id: String,
    reason: String,
}

impl ProjectionDispositionRow {
    async fn load(conn: &Connection, observation_id: &str) -> crate::errors::Result<Self> {
        let mut rows = conn
            .query(
                "SELECT receipt_id, reason FROM observation_projection_dispositions
                 WHERE projector_version = ?1 AND observation_id = ?2",
                params![SESSION_MESSAGE_PROJECTOR_VERSION_V1, observation_id],
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
        conn: &Connection,
        provider: &str,
        message_id: &str,
    ) -> crate::errors::Result<Self> {
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

    fn validate(self) -> crate::errors::Result<()> {
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
) -> crate::errors::Result<()> {
    let ObservationProjection::Message(unaliased_projection) = unaliased else {
        return Err(authority_violation(
            "projection alias is ineligible for a skipped observation",
        ));
    };
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
) -> crate::errors::Result<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    if actual.receipt_id != provenance.receipt_id()
        || actual.output_provider != message.provider
        || actual.output_message_id != message.message_id
        || actual.output_digest != projection.output_digest().as_str()
        || !matches!(actual.message_created, 0 | 1)
    {
        return Err(authority_violation(
            "projection provenance disagrees with deterministic output",
        ));
    }
    Ok(())
}

async fn validate_message_projection(
    conn: &Connection,
    observation_id: &str,
    state: ProjectionAuthorityState,
    unaliased: &ObservationProjection,
    projection: &SessionMessageProjection,
) -> crate::errors::Result<()> {
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
    let provenance = ProjectionProvenanceRow::load(conn, observation_id).await?;
    validate_provenance_row(&provenance, projection)?;
    let message = projection.message();
    ProjectionOutputOwnership::load(conn, &message.provider, &message.message_id)
        .await?
        .validate()?;
    crate::global_db::observation_projection::verify_output_authority(conn, projection)
        .await
        .map_err(|error| {
            authority_violation(format!(
                "projection output rows disagree with deterministic output: {error}"
            ))
        })
}

async fn validate_skipped_projection(
    conn: &Connection,
    observation: &DurableObservationV1,
    observation_id: &str,
    state: ProjectionAuthorityState,
    reason: ProjectionSkipReason,
) -> crate::errors::Result<()> {
    if state.is_pending_skip() {
        return Ok(());
    }
    if !state.is_skip() {
        return Err(authority_violation(
            "projection authority must contain exactly one skip outcome without an alias",
        ));
    }
    let disposition = ProjectionDispositionRow::load(conn, observation_id).await?;
    if disposition.receipt_id != observation.receipt().receipt().receipt_id().as_str()
        || disposition.reason != reason.as_str()
    {
        return Err(authority_violation(
            "projection disposition disagrees with deterministic skip reason",
        ));
    }
    Ok(())
}

async fn validate_projection_effect(
    conn: &Connection,
    observation: &DurableObservationV1,
) -> crate::errors::Result<()> {
    let unaliased = crate::global_db::observation_projection::derive_projection(observation)
        .map_err(|error| authority_violation(format!("invalid projection authority: {error}")))?;
    let effect =
        crate::global_db::observation_projection::derive_projection_with_alias(conn, observation)
            .await
            .map_err(|error| {
                authority_violation(format!("invalid projection authority: {error}"))
            })?;
    let observation_id = observation.observation_id().as_str();
    let state = ProjectionAuthorityState::load(conn, observation_id).await?;
    match effect {
        ObservationProjection::Message(projection) => {
            validate_message_projection(conn, observation_id, state, &unaliased, &projection).await
        }
        ObservationProjection::Skipped(reason) => {
            validate_skipped_projection(conn, observation, observation_id, state, reason).await
        }
    }
}

async fn observation_by_id(
    conn: &Connection,
    observation_id: &str,
) -> crate::errors::Result<DurableObservationV1> {
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
    conn: &Connection,
    table: &str,
    after_rowid: i64,
) -> crate::errors::Result<(i64, i64)> {
    let query = format!(
        "SELECT COALESCE(MAX(rowid), ?1), COUNT(*) FROM {table}
         WHERE rowid > ?1 AND projector_version = ?2"
    );
    let mut rows = conn
        .query(
            &query,
            params![after_rowid, SESSION_MESSAGE_PROJECTOR_VERSION_V1],
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

pub(super) async fn validate_projection_authority_suffix(
    conn: &Connection,
    checkpoint: AuditCheckpoint,
) -> crate::errors::Result<(AuditCheckpoint, i64, i64, i64)> {
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
    let mut rows = conn
        .query(
            "SELECT observation_id FROM observations
             WHERE sequence > ?1 AND sequence <= ?2
             UNION
             SELECT observation_id FROM observation_projection_provenance
             WHERE rowid > ?3 AND projector_version = ?5
             UNION
             SELECT observation_id FROM observation_projection_dispositions
             WHERE rowid > ?4 AND projector_version = ?5
             UNION
             SELECT observation_id FROM observation_projection_aliases
             WHERE rowid > ?6 AND projector_version = ?5",
            params![
                coverage_start,
                current_projection_checkpoint,
                checkpoint.provenance_rowid,
                checkpoint.disposition_rowid,
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
                checkpoint.alias_rowid
            ],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut observation_ids = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        observation_ids.insert(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
    }
    drop(rows);
    for observation_id in observation_ids {
        let observation = observation_by_id(conn, &observation_id).await?;
        validate_projection_effect(conn, &observation).await?;
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
    ))
}

pub(super) async fn write_audit_checkpoint(
    conn: &Connection,
    progress: AuditProgress,
) -> crate::errors::Result<()> {
    let checkpoint = progress.checkpoint;
    conn.execute(
        "INSERT INTO authority_audit_checkpoints (
            audit_name, audit_version, receipt_rowid, observation_sequence,
            provenance_rowid, disposition_rowid, alias_rowid, projection_checkpoint,
            last_receipts_audited, last_observations_audited,
            last_provenance_audited, last_dispositions_audited, last_aliases_audited,
            bounded_passes_since_exhaustive
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(audit_name) DO UPDATE SET
            audit_version = excluded.audit_version,
            receipt_rowid = excluded.receipt_rowid,
            observation_sequence = excluded.observation_sequence,
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

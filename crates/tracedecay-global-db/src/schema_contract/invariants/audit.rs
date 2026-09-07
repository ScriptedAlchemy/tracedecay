use std::collections::{BTreeSet, HashMap};

use futures_util::future::try_join_all;
use tracedecay_domain::DurableObservationV1;
use tracedecay_store::{
    ObservationProjection, ProjectionSkipReason, ProjectionStoreError,
    SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageProjection, WorkflowFactProjection,
};

use crate::observation_projection::ProjectionOutputAuthority;

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
) -> tracedecay_domain::errors::Result<()> {
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
) -> tracedecay_domain::errors::Result<()> {
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
) -> tracedecay_domain::errors::Result<Option<AuditCheckpoint>> {
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
) -> tracedecay_domain::errors::Result<bool> {
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
    #[hotpath::skip]
    async fn load(
        conn: &impl QueryExecutor,
        observation_id: &str,
    ) -> tracedecay_domain::errors::Result<Self> {
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
    #[hotpath::skip]
    async fn load(
        conn: &impl QueryExecutor,
        observation_id: &str,
    ) -> tracedecay_domain::errors::Result<Self> {
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

/// Observation ids carried by one batched provenance statement.
const PROVENANCE_ROW_BATCH_KEYS: usize = 128;

impl ProjectionProvenanceRow {
    /// Every provenance row this projector holds for a page's observations,
    /// keyed by `(observation_id, output_ordinal)`.
    ///
    /// An observation/ordinal with no row is simply absent, which each caller
    /// reads as the same "projection provenance disappeared" outcome the
    /// per-row `Ok(None)` produced.
    #[cfg_attr(
        feature = "hotpath",
        hotpath::measure(label = "global_db.observation_audit.batch.provenance")
    )]
    async fn load_batch(
        conn: &impl QueryExecutor,
        observation_ids: &BTreeSet<String>,
    ) -> tracedecay_domain::errors::Result<HashMap<(String, i64), Self>> {
        let mut rows_by_key = HashMap::new();
        if observation_ids.is_empty() {
            return Ok(rows_by_key);
        }
        let requested_keys = observation_ids.iter().collect::<Vec<_>>();
        for chunk in requested_keys.chunks(PROVENANCE_ROW_BATCH_KEYS) {
            let requested = serde_json::to_string(&chunk)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let mut rows = conn
                .query(
                    "SELECT provenance.observation_id, provenance.output_ordinal,
                            provenance.retrieval_anchor_id, provenance.receipt_id,
                            provenance.output_provider, provenance.output_message_id,
                            provenance.output_digest, provenance.message_created
                     FROM json_each(?2) AS requested
                     CROSS JOIN observation_projection_provenance AS provenance
                     WHERE provenance.projector_version = ?1
                       AND provenance.observation_id = requested.value",
                    params![SESSION_MESSAGE_PROJECTOR_VERSION, requested.as_str()],
                )
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?
            {
                let observation_id = row
                    .get::<String>(0)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?;
                let output_ordinal = row
                    .get::<i64>(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?;
                rows_by_key.insert(
                    (observation_id, output_ordinal),
                    Self {
                        retrieval_anchor_id: row
                            .get(2)
                            .map_err(|error| global_db_operation_error(OPERATION, error))?,
                        receipt_id: row
                            .get(3)
                            .map_err(|error| global_db_operation_error(OPERATION, error))?,
                        output_provider: row
                            .get(4)
                            .map_err(|error| global_db_operation_error(OPERATION, error))?,
                        output_message_id: row
                            .get(5)
                            .map_err(|error| global_db_operation_error(OPERATION, error))?,
                        output_digest: row
                            .get(6)
                            .map_err(|error| global_db_operation_error(OPERATION, error))?,
                        message_created: row
                            .get(7)
                            .map_err(|error| global_db_operation_error(OPERATION, error))?,
                    },
                );
            }
        }
        Ok(rows_by_key)
    }
}

struct ProjectionDispositionRow {
    receipt_id: String,
    reason: String,
}

impl ProjectionDispositionRow {
    #[hotpath::skip]
    async fn load(
        conn: &impl QueryExecutor,
        observation_id: &str,
    ) -> tracedecay_domain::errors::Result<Self> {
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

/// Requested output keys carried by one batched ownership statement.
const OUTPUT_OWNERSHIP_BATCH_KEYS: usize = 256;

impl ProjectionOutputOwnership {
    /// Creation-owner counts for a whole chunk of audited outputs.
    ///
    /// An output with no provenance rows never appears in the grouped result;
    /// [`ProjectionOutputOwnership::resolved`] reads that absence as zero
    /// creators, which is exactly what the per-output
    /// `COALESCE(SUM(message_created), 0)` returned for the same key. The
    /// aggregate deliberately keeps spanning every projector version, so a
    /// second projector that also claims creation is still caught.
    #[cfg_attr(
        feature = "hotpath",
        hotpath::measure(label = "global_db.observation_audit.batch.ownership")
    )]
    async fn load_batch(
        conn: &impl QueryExecutor,
        outputs: &BTreeSet<(String, String)>,
    ) -> tracedecay_domain::errors::Result<HashMap<(String, String), i64>> {
        let mut counts = HashMap::with_capacity(outputs.len());
        if outputs.is_empty() {
            return Ok(counts);
        }
        let requested_keys = outputs.iter().collect::<Vec<_>>();
        for chunk in requested_keys.chunks(OUTPUT_OWNERSHIP_BATCH_KEYS) {
            let requested = serde_json::to_string(
                &chunk
                    .iter()
                    .map(|(provider, message_id)| {
                        serde_json::json!({ "provider": provider, "message_id": message_id })
                    })
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let mut rows = conn
                .query(
                    "SELECT provenance.output_provider, provenance.output_message_id,
                            COALESCE(SUM(provenance.message_created), 0)
                     FROM observation_projection_provenance AS provenance
                     JOIN json_each(?1) AS requested
                       ON provenance.output_provider =
                            json_extract(requested.value, '$.provider')
                      AND provenance.output_message_id =
                            json_extract(requested.value, '$.message_id')
                     GROUP BY provenance.output_provider, provenance.output_message_id",
                    params![requested.as_str()],
                )
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?
            {
                let provider = row
                    .get::<String>(0)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?;
                let message_id = row
                    .get::<String>(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?;
                let creator_count = row
                    .get::<i64>(2)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?;
                counts.insert((provider, message_id), creator_count);
            }
        }
        Ok(counts)
    }

    fn resolved(counts: &HashMap<(String, String), i64>, provider: &str, message_id: &str) -> Self {
        let creator_count = counts
            .get(&(provider.to_owned(), message_id.to_owned()))
            .copied()
            .unwrap_or(0);
        Self { creator_count }
    }

    fn validate(self) -> tracedecay_domain::errors::Result<()> {
        if self.creator_count > 1 {
            return Err(authority_violation(
                "projection output has multiple creation owners",
            ));
        }
        Ok(())
    }
}

/// One chunk's stored projection authority, resolved once for every output the
/// chunk's derivations name.
///
/// The audit is a read-only convergence check over a page it has already
/// scanned: the authority every row would have read for itself, row by row, is
/// the same authority read here in one batched statement per table. What stays
/// strictly per observation is the *derivation* each row is audited against —
/// only the stored side it is compared to is shared.
///
/// `ProjectionAuthorityState` deliberately stays a per-observation read. It
/// carries the `queued` flag that decides whether an observation is audited at
/// all, so reading it earlier than the row it gates would widen exactly the
/// window in which a concurrent drain turns a completed projection back into a
/// skipped "still pending" one. The reads batched here only supply evidence a
/// row is *compared against*, never whether the comparison happens.
struct ResolvedOutputAuthority {
    authorities: HashMap<(String, String), ProjectionOutputAuthority>,
    creators: HashMap<(String, String), i64>,
    provenance: HashMap<(String, i64), ProjectionProvenanceRow>,
    projection_rows: crate::observation_projection::ProjectionRowsBatch,
}

impl ResolvedOutputAuthority {
    fn provenance_row(
        &self,
        observation_id: &str,
        output_ordinal: i64,
    ) -> Option<&ProjectionProvenanceRow> {
        self.provenance
            .get(&(observation_id.to_owned(), output_ordinal))
    }

    /// The canonical owner an output resolved to, or the same typed
    /// provenance-collision failure the single-output read raised when the
    /// output has no resolvable owner.
    fn authority(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_domain::errors::Result<&ProjectionOutputAuthority> {
        self.authorities
            .get(&(provider.to_owned(), message_id.to_owned()))
            .ok_or_else(|| {
                authority_violation(format!(
                    "projection output rows disagree with deterministic output: {}",
                    ProjectionStoreError::ProvenanceCollision
                ))
            })
    }

    fn ownership(&self, provider: &str, message_id: &str) -> ProjectionOutputOwnership {
        ProjectionOutputOwnership::resolved(&self.creators, provider, message_id)
    }
}

fn validate_alias_binding(
    alias: &ProjectionAliasRow,
    unaliased: &ObservationProjection,
    projection: &SessionMessageProjection,
) -> tracedecay_domain::errors::Result<()> {
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
) -> tracedecay_domain::errors::Result<()> {
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
    effect: &ObservationProjection,
    resolved: &ResolvedOutputAuthority,
    projection: &SessionMessageProjection,
) -> tracedecay_domain::errors::Result<bool> {
    let message = projection.message();
    let Some(provenance) =
        resolved.provenance_row(observation_id, i64::from(projection.output_ordinal()))
    else {
        return Ok(false);
    };
    validate_provenance_row(provenance, projection)?;
    resolved
        .ownership(&message.provider, &message.message_id)
        .validate()?;
    // Creating an output does not make this observation its current owner: a
    // later generation from the same source supersedes the row while the
    // historical creator keeps `message_created = 1`. Resolve the canonical
    // owner for every observation so a superseded creator is audited against
    // the projection that actually owns the row.
    let authority = resolved.authority(&message.provider, &message.message_id)?;
    let owner_projection = crate::observation_projection::resolve_output_projection(
        conn,
        authority,
        Some((observation_id, effect)),
        projection,
    )
    .await
    .map_err(|error| {
        authority_violation(format!(
            "projection output rows disagree with deterministic output: {error}"
        ))
    })?;
    let Some(owner_provenance) = resolved.provenance_row(
        &authority.canonical_observation_id,
        i64::from(owner_projection.output_ordinal()),
    ) else {
        return Ok(false);
    };
    validate_provenance_row(owner_provenance, &owner_projection)?;
    let owner_session = owner_projection.session();
    let owner_message = owner_projection.message();
    crate::observation_projection::verify_projection_rows_from_records(
        conn,
        &owner_projection,
        resolved
            .projection_rows
            .session(&owner_session.provider, &owner_session.session_id),
        resolved
            .projection_rows
            .message(&owner_message.provider, &owner_message.message_id),
    )
    .await
    .map_err(|error| {
        authority_violation(format!(
            "projection output rows disagree with deterministic output: {error}"
        ))
    })?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn validate_message_projection(
    conn: &impl QueryExecutor,
    observation_id: &str,
    state: ProjectionAuthorityState,
    unaliased: &ObservationProjection,
    effect: &ObservationProjection,
    resolved: &ResolvedOutputAuthority,
    projection: &SessionMessageProjection,
) -> tracedecay_domain::errors::Result<()> {
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
    if !validate_message_projection_row(conn, observation_id, effect, resolved, projection).await? {
        return Err(authority_violation("projection provenance disappeared"));
    }
    Ok(())
}

async fn validate_skipped_projection(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    state: ProjectionAuthorityState,
    reason: ProjectionSkipReason,
) -> tracedecay_domain::errors::Result<()> {
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
) -> tracedecay_domain::errors::Result<()> {
    if disposition.receipt_id != observation.receipt().receipt().receipt_id().as_str()
        || disposition.reason != reason.as_str()
    {
        return Err(authority_violation(
            "projection disposition disagrees with deterministic skip reason",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn validate_composite_projection(
    conn: &impl QueryExecutor,
    observation_id: &str,
    state: ProjectionAuthorityState,
    unaliased: &ObservationProjection,
    effect: &ObservationProjection,
    resolved: &ResolvedOutputAuthority,
    message: Option<&SessionMessageProjection>,
    derived_messages: &[SessionMessageProjection],
    workflow_facts: &[WorkflowFactProjection],
) -> tracedecay_domain::errors::Result<()> {
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
            validated_rows += i64::from(
                validate_message_projection_row(conn, observation_id, effect, resolved, projection)
                    .await?,
            );
        }
        for projection in derived_messages {
            validated_rows += i64::from(
                validate_message_projection_row(conn, observation_id, effect, resolved, projection)
                    .await?,
            );
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
        && !validate_message_projection_row(conn, observation_id, effect, resolved, projection)
            .await?
    {
        return Err(authority_violation("projection provenance disappeared"));
    }
    for projection in derived_messages {
        if !validate_message_projection_row(conn, observation_id, effect, resolved, projection)
            .await?
        {
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

/// Derives one observation's projection effect.
///
/// Derivation is disposition-aware, so an observation that converged to a
/// durable output-collision skip re-derives as `Skipped(OutputCollision)` and
/// audits through the `Skipped` arm natively.
async fn derive_projection_effect(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
) -> tracedecay_domain::errors::Result<ObservationProjection> {
    crate::observation_projection::derive_projection_with_alias(conn, observation)
        .await
        .map_err(|error| authority_violation(format!("invalid projection authority: {error}")))
}

/// Every `(output_provider, output_message_id)` a chunk's derivations name.
///
/// Pending outputs are included: an observation that is still queued costs one
/// extra key in the batched statement and no extra round trip, which is
/// cheaper than deciding per row whether its authority will be consulted.
fn requested_outputs(effects: &[ObservationProjection]) -> BTreeSet<(String, String)> {
    let mut outputs = BTreeSet::new();
    for effect in effects {
        for projection in effect.messages() {
            let message = projection.message();
            outputs.insert((message.provider.clone(), message.message_id.clone()));
        }
    }
    outputs
}

/// Derives a whole page's projection effects in bounded concurrent groups.
///
/// Derivation concurrency stays exactly what it was; only its result is now
/// retained so the page's stored authority can be read once for all of it.
#[cfg_attr(
    feature = "hotpath",
    hotpath::measure(label = "global_db.observation_audit.batch.derive")
)]
async fn derive_page_effects(
    conn: &impl QueryExecutor,
    observations: &[&DurableObservationV1],
) -> tracedecay_domain::errors::Result<Vec<ObservationProjection>> {
    let mut effects = Vec::with_capacity(observations.len());
    for group in observations.chunks(DETAILED_AUDIT_CONCURRENCY) {
        effects.extend(
            try_join_all(
                group
                    .iter()
                    .map(|observation| derive_projection_effect(conn, observation)),
            )
            .await?,
        );
    }
    Ok(effects)
}

/// Reads the stored authority for every output a page's derivations name, in
/// one batched statement per authority table.
#[cfg_attr(
    feature = "hotpath",
    hotpath::measure(label = "global_db.observation_audit.batch.authority")
)]
async fn resolve_output_authority(
    conn: &impl QueryExecutor,
    observations: &[&DurableObservationV1],
    effects: &[ObservationProjection],
) -> tracedecay_domain::errors::Result<ResolvedOutputAuthority> {
    let outputs = requested_outputs(effects);
    let authorities = crate::observation_projection::read_output_authorities(conn, &outputs)
        .await
        .map_err(|error| {
            authority_violation(format!(
                "projection output rows disagree with deterministic output: {error}"
            ))
        })?;
    let mut observation_ids = observations
        .iter()
        .map(|observation| observation.observation_id().as_str().to_owned())
        .collect::<BTreeSet<_>>();
    observation_ids.extend(
        authorities
            .values()
            .map(|authority| authority.canonical_observation_id.clone()),
    );
    Ok(ResolvedOutputAuthority {
        authorities,
        creators: ProjectionOutputOwnership::load_batch(conn, &outputs).await?,
        provenance: ProjectionProvenanceRow::load_batch(conn, &observation_ids).await?,
        projection_rows: crate::observation_projection::read_projection_rows_batch(conn, &outputs)
            .await
            .map_err(|error| {
                authority_violation(format!(
                    "projection output rows disagree with deterministic output: {error}"
                ))
            })?,
    })
}

/// Validates one bounded set of audited observations: derive, resolve the
/// stored authority once, then validate each derivation against it. Every
/// per-row invariant is unchanged; only the number of round trips spent
/// re-reading the same stored authority is.
async fn validate_projection_effects(
    conn: &impl QueryExecutor,
    observations: &[&DurableObservationV1],
) -> tracedecay_domain::errors::Result<()> {
    let effects = derive_page_effects(conn, observations).await?;
    let resolved = resolve_output_authority(conn, observations, &effects).await?;
    for group in observations
        .chunks(DETAILED_AUDIT_CONCURRENCY)
        .zip(effects.chunks(DETAILED_AUDIT_CONCURRENCY))
        .map(|(observations, effects)| observations.iter().zip(effects))
    {
        try_join_all(group.map(|(observation, effect)| {
            validate_projection_effect(conn, observation, effect, &resolved)
        }))
        .await?;
    }
    Ok(())
}

async fn validate_projection_effect(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    effect: &ObservationProjection,
    resolved: &ResolvedOutputAuthority,
) -> tracedecay_domain::errors::Result<()> {
    // The unaliased projection is only needed to validate alias bindings, so it
    // is derived lazily inside the arms that consume it.
    let observation_id = observation.observation_id().as_str();
    let state = ProjectionAuthorityState::load(conn, observation_id).await?;
    match effect {
        ObservationProjection::Message(projection) => {
            if state.workflow_rows != 0 {
                return Err(authority_violation(
                    "message projection contains unexpected workflow output",
                ));
            }
            let unaliased = derive_unaliased_projection(observation)?;
            validate_message_projection(
                conn,
                observation_id,
                state,
                &unaliased,
                effect,
                resolved,
                projection,
            )
            .await
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
                effect,
                resolved,
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
) -> tracedecay_domain::errors::Result<ObservationProjection> {
    crate::observation_projection::derive_projection(observation)
        .map_err(|error| authority_violation(format!("invalid projection authority: {error}")))
}

async fn observation_by_id(
    conn: &impl QueryExecutor,
    observation_id: &str,
) -> tracedecay_domain::errors::Result<DurableObservationV1> {
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
) -> tracedecay_domain::errors::Result<(i64, i64)> {
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
) -> tracedecay_domain::errors::Result<()> {
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
) -> tracedecay_domain::errors::Result<i64> {
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
) -> tracedecay_domain::errors::Result<AuditCheckpoint> {
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
) -> tracedecay_domain::errors::Result<(AuditCheckpoint, i64, i64, i64, bool)> {
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
            let stored_rejection = disposition
                .as_ref()
                .and_then(|disposition| ProjectionSkipReason::from_durable_str(&disposition.reason))
                .is_some_and(|reason| {
                    matches!(
                        reason,
                        ProjectionSkipReason::OutputCollision
                            | ProjectionSkipReason::InvalidContract
                    )
                });
            if stored_rejection {
                if !state.is_skip() {
                    return Err(authority_violation(
                        "projection authority must contain exactly one deterministic skip outcome",
                    ));
                }
                let disposition = disposition
                    .as_ref()
                    .ok_or_else(|| authority_violation("projection disposition disappeared"))?;
                if disposition.receipt_id != observation_receipt_id {
                    return Err(authority_violation(
                        "projection disposition disagrees with observation receipt",
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
        // Derivation and the page's stored authority are read once for the
        // whole page; validation still advances the checkpoint in
        // `validation_concurrency` chunks, so batching the reads never widens
        // the stride an interruption has to replay.
        let page_observations = detailed_observations
            .iter()
            .map(|(_, observation)| observation)
            .collect::<Vec<_>>();
        let page_effects = derive_page_effects(conn, &page_observations).await?;
        let resolved = resolve_output_authority(conn, &page_observations, &page_effects).await?;
        for (chunk, chunk_effects) in detailed_observations
            .chunks(validation_concurrency)
            .zip(page_effects.chunks(validation_concurrency))
        {
            try_join_all(
                chunk
                    .iter()
                    .zip(chunk_effects)
                    .map(|((_, observation), effect)| {
                        validate_projection_effect(conn, observation, effect, &resolved)
                    }),
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
            let chunk_observations = chunk.iter().collect::<Vec<_>>();
            validate_projection_effects(conn, &chunk_observations).await?;
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
) -> tracedecay_domain::errors::Result<(AuditCheckpoint, i64, i64, i64)> {
    let (checkpoint, provenance, dispositions, aliases, _) =
        validate_projection_authority_suffix_pages(conn, checkpoint, None).await?;
    Ok((checkpoint, provenance, dispositions, aliases))
}

pub(super) async fn validate_projection_authority_chunk(
    conn: &impl QueryExecutor,
    checkpoint: AuditCheckpoint,
) -> tracedecay_domain::errors::Result<(AuditCheckpoint, i64, i64, i64, bool)> {
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
) -> tracedecay_domain::errors::Result<()> {
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
        AuditCheckpoint, BTreeSet, DETAILED_AUDIT_CHUNKS_PER_PAGE, DETAILED_AUDIT_CONCURRENCY,
        DETAILED_TAIL_CONCURRENCY, HashMap, MAX_DETAILED_OBSERVATIONS_PER_PAGE,
        PROJECTION_PROGRESS_PAGE_INTERVAL, ProjectionOutputOwnership, ResolvedOutputAuthority,
        ensure_audit_checkpoint_schema, historical_projection_delta_required,
        projection_audit_checkpoint_through_sequence, validate_projection_authority_suffix,
    };
    use crate::tests::harness::{RegisteredGlobalDbTestFixture, open_registered_test_fixture};
    use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
    use tracedecay_runtime_core::db::engine::{
        Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows, TestConnection, params,
    };

    struct CountingQuery<'a> {
        inner: &'a RegisteredGlobalDbTestFixture,
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
        let connection = open_registered_test_fixture(
            &directory.path().join("sessions.db"),
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();

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
        let connection = open_registered_test_fixture(
            &directory.path().join("sessions.db"),
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();
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
        let connection = open_registered_test_fixture(
            &directory.path().join("sessions.db"),
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();
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

    /// Seeds one retained observation row (and the receipt it references) so the
    /// batched authority resolver has real `observations` rows to order and
    /// decode. The observation's own projection outcome is irrelevant here: the
    /// resolver only selects and decodes the owner row.
    async fn seed_authority_observation(
        conn: &impl Executor,
        index: usize,
    ) -> DurableObservationV1 {
        let observation = skipped_observation(index);
        let receipt = observation.receipt();
        conn.execute(
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
        conn.execute(
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
        // Session-projector provenance must bind a retrieval anchor owned by
        // the same observation and receipt; the binding triggers refuse an
        // unbound row for every `claude-session-message-v*` projector.
        let anchor_id = audit_anchor_id(&observation);
        conn.execute(
            "INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES (?1, '{\"kind\":\"audit\"}', '{\"owner\":\"audit\"}', 'projection.gen.v1')",
            params![anchor_id.as_str()],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO observation_retrieval_anchors (observation_id, anchor_id)
             VALUES (?1, ?2)",
            params![observation.observation_id().as_str(), anchor_id.as_str()],
        )
        .await
        .unwrap();
        observation
    }

    fn audit_anchor_id(observation: &DurableObservationV1) -> String {
        format!("anchor.{}", observation.observation_id().as_str())
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_provenance(
        conn: &impl Executor,
        projector_version: &str,
        observation: &DurableObservationV1,
        output_ordinal: i64,
        output_provider: &str,
        output_message_id: &str,
        message_created: i64,
    ) {
        conn.execute(
            "INSERT INTO observation_projection_provenance (
                projector_version, observation_id, output_ordinal, receipt_id,
                output_provider, output_message_id, output_digest, message_created,
                retrieval_anchor_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                projector_version,
                observation.observation_id().as_str(),
                output_ordinal,
                observation.receipt().receipt().receipt_id().as_str(),
                output_provider,
                output_message_id,
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                message_created,
                audit_anchor_id(observation)
            ],
        )
        .await
        .unwrap();
    }

    const AUDIT_PROVIDER: &str = "codex";

    /// The batched resolver must reproduce the projector's ownership ordering
    /// exactly: an output the projector created resolves to its *newest* owner,
    /// an output it only adopted resolves to its *oldest*, and a requested key
    /// with no provenance resolves to nothing at all (the absence every caller
    /// turns into a typed provenance collision).
    #[tokio::test]
    async fn batched_output_authority_reproduces_projector_ownership_ordering() {
        let directory = TempDir::new().unwrap();
        let connection = open_registered_test_fixture(
            &directory.path().join("sessions.db"),
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();

        let first = seed_authority_observation(&connection, 0).await;
        let second = seed_authority_observation(&connection, 1).await;
        let third = seed_authority_observation(&connection, 2).await;

        // "created" is owned by the projector (one row carries message_created),
        // so the newest observation wins.
        seed_provenance(
            &connection,
            SESSION_MESSAGE_PROJECTOR_VERSION,
            &first,
            0,
            AUDIT_PROVIDER,
            "created",
            1,
        )
        .await;
        seed_provenance(
            &connection,
            SESSION_MESSAGE_PROJECTOR_VERSION,
            &third,
            0,
            AUDIT_PROVIDER,
            "created",
            0,
        )
        .await;
        // "adopted" was never created by this projector, so the oldest wins.
        seed_provenance(
            &connection,
            SESSION_MESSAGE_PROJECTOR_VERSION,
            &second,
            0,
            AUDIT_PROVIDER,
            "adopted",
            0,
        )
        .await;
        seed_provenance(
            &connection,
            SESSION_MESSAGE_PROJECTOR_VERSION,
            &third,
            1,
            AUDIT_PROVIDER,
            "adopted",
            0,
        )
        .await;
        // A different projector's rows must never leak into this projector's
        // resolution.
        seed_provenance(
            &connection,
            "session-message.other",
            &first,
            0,
            AUDIT_PROVIDER,
            "foreign",
            1,
        )
        .await;

        let requested = BTreeSet::from([
            (AUDIT_PROVIDER.to_owned(), "created".to_owned()),
            (AUDIT_PROVIDER.to_owned(), "adopted".to_owned()),
            (AUDIT_PROVIDER.to_owned(), "foreign".to_owned()),
            (AUDIT_PROVIDER.to_owned(), "never-projected".to_owned()),
        ]);
        let resolved =
            crate::observation_projection::read_output_authorities(&connection, &requested)
                .await
                .unwrap();

        assert_eq!(
            resolved
                .get(&(AUDIT_PROVIDER.to_owned(), "created".to_owned()))
                .map(|authority| authority.canonical_observation_id.as_str()),
            Some(third.observation_id().as_str()),
            "a projector-owned output resolves to its newest owner"
        );
        assert_eq!(
            resolved
                .get(&(AUDIT_PROVIDER.to_owned(), "adopted".to_owned()))
                .map(|authority| authority.canonical_observation_id.as_str()),
            Some(second.observation_id().as_str()),
            "an adopted output resolves to its oldest owner"
        );
        assert!(
            !resolved.contains_key(&(AUDIT_PROVIDER.to_owned(), "foreign".to_owned())),
            "another projector's provenance must not resolve this projector's authority"
        );
        assert!(
            !resolved.contains_key(&(AUDIT_PROVIDER.to_owned(), "never-projected".to_owned())),
            "an output with no provenance must resolve to nothing"
        );
        // The decoded owner is the retained observation itself, not a
        // re-derivation of it.
        assert_eq!(
            resolved
                .get(&(AUDIT_PROVIDER.to_owned(), "adopted".to_owned()))
                .map(|authority| authority.canonical.observation_id().as_str()),
            Some(second.observation_id().as_str())
        );
    }

    /// Every requested key must be answered, including across the chunk
    /// boundary the batched statement pages at.
    #[tokio::test]
    async fn batched_output_authority_answers_every_key_across_chunks() {
        const OUTPUTS: usize = 300;

        let directory = TempDir::new().unwrap();
        let connection = open_registered_test_fixture(
            &directory.path().join("sessions.db"),
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();

        let mut expected = Vec::with_capacity(OUTPUTS);
        let mut requested = BTreeSet::new();
        for index in 0..OUTPUTS {
            let observation = seed_authority_observation(&connection, index).await;
            let output = format!("output-{index:04}");
            seed_provenance(
                &connection,
                SESSION_MESSAGE_PROJECTOR_VERSION,
                &observation,
                0,
                AUDIT_PROVIDER,
                &output,
                1,
            )
            .await;
            requested.insert((AUDIT_PROVIDER.to_owned(), output));
            expected.push(observation.observation_id().as_str().to_owned());
        }

        let resolved =
            crate::observation_projection::read_output_authorities(&connection, &requested)
                .await
                .unwrap();

        assert_eq!(resolved.len(), OUTPUTS);
        for (index, observation_id) in expected.iter().enumerate() {
            let key = (AUDIT_PROVIDER.to_owned(), format!("output-{index:04}"));
            assert_eq!(
                resolved
                    .get(&key)
                    .map(|authority| authority.canonical_observation_id.as_str()),
                Some(observation_id.as_str()),
                "chunked authority resolution dropped {key:?}"
            );
        }
    }

    /// Counts the read round trips one read-only audit pass spends.
    struct CountingSnapshot<'a, T> {
        inner: &'a T,
        queries: AtomicUsize,
        statements: std::sync::Mutex<Vec<String>>,
    }

    impl<'a, T> CountingSnapshot<'a, T> {
        fn new(inner: &'a T) -> Self {
            Self {
                inner,
                queries: AtomicUsize::new(0),
                statements: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// How many statements carrying `needle` were issued.
        fn issued(&self, needle: &str) -> usize {
            self.statements
                .lock()
                .unwrap()
                .iter()
                .filter(|sql| sql.contains(needle))
                .count()
        }

        fn statement_containing(&self, needle: &str) -> String {
            self.statements
                .lock()
                .unwrap()
                .iter()
                .find(|sql| sql.contains(needle))
                .cloned()
                .unwrap_or_else(|| panic!("no captured statement contained {needle:?}"))
        }
    }

    impl<T: QueryExecutor> QueryExecutor for CountingSnapshot<'_, T> {
        async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
        where
            P: IntoParams,
        {
            self.queries.fetch_add(1, Ordering::Relaxed);
            self.statements.lock().unwrap().push(sql.to_owned());
            self.inner.query(sql, params).await
        }
    }

    /// One session's projected messages, committed and drained through the real
    /// observation store so the audit sees production-shaped authority.
    async fn seed_projected_messages(
        runtime: &crate::tests::harness::HostAdmissionTestRuntimeV1,
        count: usize,
    ) -> Vec<DurableObservationV1> {
        use tracedecay_domain::{
            CanonicalMessageRoleV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
            CanonicalObservationRelationsV1, ObservationSourceCursorV1, ProjectionGenerationId,
            ProviderId, SessionId, UtcMicros,
        };
        use tracedecay_store::{
            AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
            ObservationStore, ObservationWrite,
        };

        let store = runtime
            .observation_store(crate::tests::harness::HostAdmissionScope::Profile)
            .unwrap();
        let provider = ProviderId::new("codex").unwrap();
        let session_id = SessionId::new("session.audit-batch").unwrap();
        let source =
            ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
                .unwrap();
        let mut expected_cursor: Option<ObservationSourceCursorV1> = None;
        let mut observations = Vec::with_capacity(count);
        for index in 0..count {
            let record_id = format!("record.audit-batch-{index}");
            let record = ObservationId::new(record_id.clone()).unwrap();
            let start = u64::try_from(index).unwrap() * 100;
            let range = ObservationSourceRangeV1::new(start, start + 100).unwrap();
            let relations = CanonicalObservationRelationsV1::new(session_id.clone())
                .with_message_id(ObservationId::new(format!("message.{record_id}")).unwrap());
            let envelope = CanonicalObservationEnvelopeV1::new(
                provider.clone(),
                "message",
                record.clone(),
                relations,
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::Assistant,
                    content: serde_json::json!({ "text": format!("audit batch {index}") }),
                    model: None,
                    timestamp: Some(1_750_000_000),
                }],
                CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
            )
            .unwrap();
            let payload = serde_json::to_value(envelope).unwrap();
            let receipt = SanitizationReceiptV1::new(
                SanitizationReceiptRefV1::new(
                    SanitizationReceiptId::new(format!("receipt.audit-batch-{index}")).unwrap(),
                    ComponentVersion::new("sanitizer.audit-batch.v1").unwrap(),
                )
                .unwrap(),
                SanitizerDispositionV1::Accepted,
                SensitivityV1::NonSensitive,
                Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
            )
            .unwrap();
            let observation = DurableObservationV1::new(
                ObservationIdentityMaterialV1::for_native_record(
                    source.clone(),
                    ObservationScopeV1::Profile,
                    ObservationSourceGenerationV1::new(1).unwrap(),
                    range,
                    ObservationOrderingDomainV1::FileBytes,
                    record,
                )
                .unwrap(),
                receipt,
                RetentionClass::new("retention.audit-batch").unwrap(),
                payload,
            )
            .unwrap();
            let next_cursor = ObservationSourceCursorV1::for_ordering(
                observation.source().clone(),
                observation.scope().clone(),
                observation.identity().generation(),
                observation.identity().ordering_domain(),
                observation.identity().position().end(),
            )
            .unwrap();
            let write = ObservationWrite::new(
                observation.clone(),
                expected_cursor.clone(),
                next_cursor.clone(),
            )
            .unwrap();
            let generation = ProjectionGenerationId::new("projection.audit-batch.v1").unwrap();
            let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
                write.observation(),
                "audit-batch",
            )
            .unwrap();
            let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
                write.observation(),
                generation.clone(),
                UtcMicros(1),
                authorization,
            )
            .unwrap();
            let anchored = AnchoredObservationWrite::new(write, anchor, generation).unwrap();
            assert!(matches!(
                store.persist_observation(anchored).await.unwrap(),
                ObservationPersistOutcome::Committed(_)
            ));
            store
                .project_observation(observation.observation_id())
                .await
                .unwrap();
            expected_cursor = Some(next_cursor);
            observations.push(observation);
        }
        observations
    }

    /// The audit's message path must stay correct while resolving each chunk's
    /// output authority once instead of per projected message. The per-row
    /// baseline this replaced issued two authority reads, one ownership read
    /// and a full owner re-derivation (two more reads) for every message on top
    /// of the rest; the batched path must land well under that.
    #[tokio::test]
    async fn projection_audit_batches_message_output_authority() {
        const OBSERVATIONS: usize = 24;

        let directory = TempDir::new().unwrap();
        let runtime = crate::tests::harness::HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .unwrap();
        let observations = seed_projected_messages(&runtime, OBSERVATIONS).await;

        let database = runtime
            .registered_database(crate::tests::harness::HostAdmissionScope::Profile)
            .expect("registered profile database");
        let snapshot = database.read_snapshot().await.unwrap();
        let counting = CountingSnapshot::new(&snapshot);
        validate_projection_authority_suffix(&counting, AuditCheckpoint::default())
            .await
            .expect("a batched audit of well-formed projections must converge");

        // Each batched authority read is issued once for the whole page, not
        // once per projected message.
        assert_eq!(
            counting.issued("MAX(provenance.message_created) AS projector_owned"),
            1,
            "output ownership must resolve once per page"
        );
        assert_eq!(
            counting.issued("COALESCE(SUM(provenance.message_created), 0)"),
            1,
            "creation-owner counting must resolve once per page"
        );
        assert_eq!(
            counting.issued("SELECT provenance.observation_id, provenance.output_ordinal"),
            1,
            "provenance rows must be read once per page"
        );
        assert_eq!(
            counting.issued(
                "SELECT retrieval_anchor_id, receipt_id, output_provider, output_message_id",
            ),
            0,
            "canonical-owner provenance must reuse the page authority instead of reading per output"
        );
        assert_eq!(
            counting.issued("FROM sessions WHERE provider = ?1 AND session_id = ?2"),
            0,
            "projected sessions must reuse the page authority instead of reading per output"
        );
        assert_eq!(
            counting.issued("FROM session_messages WHERE provider = ?1 AND message_id = ?2"),
            0,
            "projected messages must reuse the page authority instead of reading per output"
        );

        let authority_sql =
            counting.statement_containing("MAX(provenance.message_created) AS projector_owned");
        let requested = serde_json::to_string(&[serde_json::json!({
            "provider": "codex",
            "message_id": "message.record.audit-batch-0"
        })])
        .unwrap();
        let mut rows = snapshot
            .query(
                &format!("EXPLAIN QUERY PLAN {authority_sql}"),
                params![SESSION_MESSAGE_PROJECTOR_VERSION, requested],
            )
            .await
            .unwrap();
        let mut plan = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            plan.push(row.get::<String>(3).unwrap());
        }
        assert!(
            plan.iter().any(|detail| {
                detail.contains("idx_observation_projection_provenance_output")
                    && detail.contains(
                        "projector_version=? AND output_provider=? AND output_message_id=?",
                    )
            }),
            "authority lookup must seek each requested output through the exact index: {plan:?}"
        );
        assert!(
            !plan.iter().any(|detail| {
                detail.contains("idx_observation_projection_provenance_output")
                    && detail.ends_with("(projector_version=?)")
            }),
            "authority lookup regressed to a projector-wide provenance scan: {plan:?}"
        );

        let provenance_sql = counting
            .statement_containing("SELECT provenance.observation_id, provenance.output_ordinal");
        let requested = serde_json::to_string(&["observation.audit-batch-0"]).unwrap();
        let mut rows = snapshot
            .query(
                &format!("EXPLAIN QUERY PLAN {provenance_sql}"),
                params![SESSION_MESSAGE_PROJECTOR_VERSION, requested],
            )
            .await
            .unwrap();
        let mut plan = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            plan.push(row.get::<String>(3).unwrap());
        }
        assert!(
            plan.iter().any(|detail| {
                detail.contains("observation_projection_provenance")
                    && detail.contains("projector_version=? AND observation_id=?")
            }),
            "provenance lookup must seek each requested observation: {plan:?}"
        );
        assert!(
            !plan.iter().any(|detail| {
                detail.contains("observation_projection_provenance")
                    && detail.ends_with("(projector_version=?)")
            }),
            "provenance lookup regressed to a projector-wide scan: {plan:?}"
        );
        // The per-row baseline this replaced spent fifteen reads on every
        // projected message; the batched path must stay comfortably under ten.
        let queries = counting.queries.load(Ordering::Relaxed);
        assert!(
            queries < OBSERVATIONS * 10,
            "projection audit issued {queries} queries for {OBSERVATIONS} projected messages"
        );

        let effect = crate::observation_projection::derive_projection(&observations[0]).unwrap();
        let projection = effect.message().expect("seeded message projection");
        let message = projection.message();
        let requested = BTreeSet::from([(message.provider.clone(), message.message_id.clone())]);
        let authority =
            crate::observation_projection::read_output_authorities(&snapshot, &requested)
                .await
                .unwrap();
        let authority = authority
            .get(&(message.provider.clone(), message.message_id.clone()))
            .expect("seeded output authority");
        let verification = CountingSnapshot::new(&snapshot);
        let owner_projection = crate::observation_projection::resolve_output_projection(
            &verification,
            authority,
            None,
            projection,
        )
        .await
        .unwrap();
        crate::observation_projection::verify_projection_rows(&verification, &owner_projection)
            .await
            .unwrap();
        assert_eq!(
            verification.issued("SELECT reason FROM observation_projection_dispositions"),
            0,
            "an unaliased retained message must not re-query its disposition"
        );
        assert_eq!(
            verification.issued("FROM observation_projection_aliases"),
            0,
            "an unaliased retained message must not re-query its alias"
        );
    }

    /// Batched creation-owner counting keeps the "one creator per output"
    /// invariant, including the cross-projector case a single projector-scoped
    /// query would miss, and reads an unknown output as zero creators.
    #[tokio::test]
    async fn batched_output_ownership_still_rejects_multiple_creators() {
        let directory = TempDir::new().unwrap();
        let connection = open_registered_test_fixture(
            &directory.path().join("sessions.db"),
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();

        let first = seed_authority_observation(&connection, 0).await;
        let second = seed_authority_observation(&connection, 1).await;
        seed_provenance(
            &connection,
            SESSION_MESSAGE_PROJECTOR_VERSION,
            &first,
            0,
            AUDIT_PROVIDER,
            "contested",
            1,
        )
        .await;
        seed_provenance(
            &connection,
            "session-message.other",
            &second,
            0,
            AUDIT_PROVIDER,
            "contested",
            1,
        )
        .await;
        seed_provenance(
            &connection,
            SESSION_MESSAGE_PROJECTOR_VERSION,
            &first,
            1,
            AUDIT_PROVIDER,
            "single",
            1,
        )
        .await;

        let requested = BTreeSet::from([
            (AUDIT_PROVIDER.to_owned(), "contested".to_owned()),
            (AUDIT_PROVIDER.to_owned(), "single".to_owned()),
        ]);
        let creators = ProjectionOutputOwnership::load_batch(&connection, &requested)
            .await
            .unwrap();
        let resolved = ResolvedOutputAuthority {
            authorities: crate::observation_projection::read_output_authorities(
                &connection,
                &requested,
            )
            .await
            .unwrap(),
            creators,
            provenance: HashMap::new(),
            projection_rows: crate::observation_projection::read_projection_rows_batch(
                &connection,
                &requested,
            )
            .await
            .unwrap(),
        };

        let contested = resolved
            .ownership(AUDIT_PROVIDER, "contested")
            .validate()
            .unwrap_err();
        assert!(
            contested.to_string().contains("multiple creation owners"),
            "{contested}"
        );
        resolved
            .ownership(AUDIT_PROVIDER, "single")
            .validate()
            .unwrap();
        // An output nobody projected reads as zero creators, exactly what the
        // per-output `COALESCE(SUM(...), 0)` returned.
        resolved
            .ownership(AUDIT_PROVIDER, "never-projected")
            .validate()
            .unwrap();
        // ... but it has no resolvable owner, so the audit still raises the
        // typed provenance collision the single-output read raised.
        let missing = resolved
            .authority(AUDIT_PROVIDER, "never-projected")
            .unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("projection output rows disagree with deterministic output"),
            "{missing}"
        );
    }
}

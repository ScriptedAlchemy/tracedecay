use std::sync::atomic::AtomicBool;

use tracedecay_store::{
    ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SESSION_MESSAGE_PROJECTOR_VERSION_V2, SESSION_MESSAGE_PROJECTOR_VERSION_V3,
    SESSION_MESSAGE_PROJECTOR_VERSION_V4,
};

use crate::db::engine::{Connection, Executor, QueryExecutor, TransactionBehavior, params};

use super::apply::{apply_effect, derive_projection_with_alias, seed_predecessor_message_lineage};
use super::rebuild::{
    prepare_projection_rebuild_with_engine, projection_rebuild_pending, read_observation_frontier,
    resume_projection_rebuild_with_engine,
};
use super::state::{
    consume_projection_queue_item, decode_observation_row, decode_sequence,
    ensure_projection_output_state_cache, inherit_predecessor_output_state, storage,
    storage_message, write_checkpoint,
};

const MIGRATION_PAGE_SIZE: i64 = 128;

struct PredecessorFrontier {
    version: String,
    sequence: u64,
}

struct MigrationProgress {
    migrated_through: u64,
    completed: bool,
}

/// Runs projector migration through an already-attached runtime connection.
///
/// The caller must supply the connection from the exact registered owner
/// binding. This function never resolves or opens a database path; transaction
/// begin, every write, and commit remain subject to the runtime's actor-time
/// write-authority checks.
pub async fn prepare_projection_version_migration_with_engine(
    conn: &Connection,
) -> ProjectionStoreResult<()> {
    if !MIGRATION_TARGET_IS_REGISTERED {
        return Ok(());
    }
    if projection_rebuild_pending(conn).await? {
        return Ok(());
    }
    if !projection_version_migration_pending(conn).await? {
        return Ok(());
    }

    match migrate_projection_page_with_engine(conn).await? {
        MigrationPageOutcome::Advanced => Ok(()),
        MigrationPageOutcome::UnmigratableLineage => {
            rebuild_instead_of_migrating_with_engine(conn).await
        }
    }
}

pub async fn advance_projection_version_migration_until_cancelled_with_engine(
    conn: &Connection,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<bool> {
    if !MIGRATION_TARGET_IS_REGISTERED {
        return Ok(true);
    }
    if let Some(complete) = resume_projection_rebuild_with_engine(conn, cancelled).await? {
        finish_projection_rebuild_migration(conn, complete).await?;
    } else {
        prepare_projection_version_migration_with_engine(conn).await?;
    }
    projection_version_migration_complete_with_engine(conn).await
}

pub async fn projection_version_migration_complete_with_engine(
    conn: &Connection,
) -> ProjectionStoreResult<bool> {
    if !MIGRATION_TARGET_IS_REGISTERED {
        return Ok(true);
    }
    if projection_rebuild_pending(conn).await? {
        return Ok(false);
    }
    projection_version_migration_pending(conn)
        .await
        .map(|pending| !pending)
}

/// Every projector version this module can converge a store to, paired with
/// whether reaching it requires migrating a predecessor frontier.
///
/// Bumping `SESSION_MESSAGE_PROJECTOR_VERSION` without adding its entry here
/// fails the build. It used to fail the daemon instead: the check ran on every
/// store open and turned a missing migration into a runtime open error, which
/// is the worst possible place to learn that a constant changed.
const REGISTERED_MIGRATION_TARGETS: [(&str, bool); 2] = [
    // The origin version. Nothing precedes it, so it needs no migration.
    (SESSION_MESSAGE_PROJECTOR_VERSION_V1, false),
    // Migrates incrementally from a V1, V2, or V3 predecessor frontier; see
    // `read_predecessor_frontier`.
    (SESSION_MESSAGE_PROJECTOR_VERSION_V4, true),
];

const fn const_str_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn migration_required_for(version: &str) -> bool {
    let mut index = 0;
    while index < REGISTERED_MIGRATION_TARGETS.len() {
        let (candidate, required) = REGISTERED_MIGRATION_TARGETS[index];
        if const_str_eq(candidate, version) {
            return required;
        }
        index += 1;
    }
    panic!(
        "SESSION_MESSAGE_PROJECTOR_VERSION has no entry in REGISTERED_MIGRATION_TARGETS; \
         add the new version and its migration before bumping the constant"
    );
}

/// Resolved at compile time from the table above.
const MIGRATION_TARGET_IS_REGISTERED: bool =
    migration_required_for(SESSION_MESSAGE_PROJECTOR_VERSION);

/// Converges a store whose predecessor projection lineage cannot support the
/// incremental version migration (sequence gaps, or observations with zero or
/// several predecessor outcomes — states an interrupted or older writer can
/// leave behind). Projections are derived data: rebuild the current version
/// from canonical observations instead of failing the open forever, then mark
/// the incremental migration superseded so it stops re-arming.
async fn rebuild_instead_of_migrating_with_engine(conn: &Connection) -> ProjectionStoreResult<()> {
    tracing::warn!(
        target_version = SESSION_MESSAGE_PROJECTOR_VERSION,
        "projection version migration fell back to a full rebuild"
    );
    let frontier = read_observation_frontier(conn).await?;
    prepare_projection_rebuild_with_engine(conn, frontier).await
}

async fn finish_projection_rebuild_migration(
    conn: &Connection,
    complete: bool,
) -> ProjectionStoreResult<()> {
    if !complete {
        return Ok(());
    }
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| storage("begin projection migration supersession", error))?;
    record_projection_migration_supersession(&transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection migration supersession", error))
}

async fn record_projection_migration_supersession(
    transaction: &impl Executor,
) -> ProjectionStoreResult<()> {
    let Some(predecessor) = read_predecessor_frontier(transaction).await? else {
        return Ok(());
    };
    let frontier_i64 = i64::try_from(predecessor.sequence).map_err(|_| {
        storage_message(
            "record projection migration supersession",
            "sequence overflow",
        )
    })?;
    let changed = transaction
        .execute(
            "INSERT INTO observation_projection_migrations (
                source_projector_version, target_projector_version,
                source_frontier, migrated_through, completed
             ) VALUES (?1, ?2, ?3, ?3, 1)
             ON CONFLICT(source_projector_version, target_projector_version)
             DO UPDATE SET migrated_through = excluded.migrated_through, completed = 1
             WHERE observation_projection_migrations.source_frontier =
                   excluded.source_frontier",
            params![
                predecessor.version.as_str(),
                SESSION_MESSAGE_PROJECTOR_VERSION,
                frontier_i64
            ],
        )
        .await
        .map_err(|error| storage("record projection migration supersession", error))?;
    if changed != 1 {
        return Err(storage_message(
            "record projection migration supersession",
            "migration source frontier changed before supersession",
        ));
    }
    Ok(())
}

/// One incremental migration page: either it advanced (more pages may
/// remain; the next open continues), or the predecessor lineage disqualified
/// incremental migration entirely.
#[derive(Clone, Copy)]
pub(super) enum MigrationPageOutcome {
    Advanced,
    UnmigratableLineage,
}

async fn migrate_projection_page_with_engine(
    conn: &Connection,
) -> ProjectionStoreResult<MigrationPageOutcome> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| storage("begin projection version migration page", error))?;
    let outcome = migrate_projection_page_transaction(&transaction).await?;
    if matches!(outcome, MigrationPageOutcome::UnmigratableLineage) {
        return Ok(outcome);
    }
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection version migration page", error))?;
    Ok(outcome)
}

async fn migrate_projection_page_transaction(
    transaction: &impl Executor,
) -> ProjectionStoreResult<MigrationPageOutcome> {
    let Some(predecessor) = read_predecessor_frontier(transaction).await? else {
        return Ok(MigrationPageOutcome::Advanced);
    };

    transaction
        .execute(
            "INSERT OR IGNORE INTO observation_projection_migrations (
                source_projector_version, target_projector_version,
                source_frontier, migrated_through, completed
             ) VALUES (?1, ?2, ?3, 0, 0)",
            params![
                predecessor.version.as_str(),
                SESSION_MESSAGE_PROJECTOR_VERSION,
                i64::try_from(predecessor.sequence).map_err(|_| storage_message(
                    "initialize projection version migration",
                    "sequence overflow"
                ))?
            ],
        )
        .await
        .map_err(|error| storage("initialize projection version migration", error))?;
    let progress = read_migration_progress(transaction, &predecessor)
        .await?
        .ok_or_else(|| {
            storage_message(
                "read projection version migration",
                "initialized migration watermark disappeared",
            )
        })?;
    if progress.completed {
        return Ok(MigrationPageOutcome::Advanced);
    }

    ensure_projection_output_state_cache(transaction).await?;
    let mut migrated_frontier = progress.migrated_through;
    if migrated_frontier >= predecessor.sequence {
        return Err(storage_message(
            "read projection version migration",
            "incomplete migration watermark reached its source frontier",
        ));
    }
    let migrated_frontier_i64 = i64::try_from(migrated_frontier)
        .map_err(|_| storage_message("migrate projection frontier", "sequence overflow"))?;
    let predecessor_frontier_i64 = i64::try_from(predecessor.sequence)
        .map_err(|_| storage_message("migrate projection frontier", "sequence overflow"))?;
    let mut rows = transaction
        .query(
            "SELECT observation.sequence, observation.observation_json,
                    (EXISTS (
                        SELECT 1 FROM observation_projection_provenance AS predecessor
                        WHERE predecessor.observation_id = observation.observation_id
                          AND predecessor.projector_version = ?1
                     ) + EXISTS (
                        SELECT 1 FROM observation_projection_dispositions AS predecessor
                        WHERE predecessor.observation_id = observation.observation_id
                          AND predecessor.projector_version = ?1
                     )) AS predecessor_outcomes
             FROM observations AS observation
             WHERE observation.sequence > ?2 AND observation.sequence <= ?3
             ORDER BY observation.sequence, observation.observation_id
             LIMIT ?4",
            params![
                predecessor.version.as_str(),
                migrated_frontier_i64,
                predecessor_frontier_i64,
                MIGRATION_PAGE_SIZE
            ],
        )
        .await
        .map_err(|error| storage("read predecessor projection authority", error))?;
    let mut page = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read predecessor projection authority", error))?
    {
        let decoded = decode_observation_row(&row, "decode predecessor projection observation")?;
        let predecessor_outcomes = row
            .get::<i64>(2)
            .map_err(|error| storage("decode predecessor projection authority", error))?;
        page.push((decoded, predecessor_outcomes));
    }
    drop(rows);
    if page.is_empty() {
        return Err(storage_message(
            "migrate projection frontier",
            "predecessor checkpoint crosses a missing observation sequence",
        ));
    }

    let page_last_sequence = page
        .last()
        .map(|((sequence, _), _)| *sequence)
        .ok_or_else(|| storage_message("migrate projection frontier", "empty migration page"))?;
    let mut expected_sequence = migrated_frontier.saturating_add(1);
    for ((sequence, _), predecessor_outcomes) in &page {
        if *sequence != expected_sequence || *predecessor_outcomes != 1 {
            // Reject incompatible predecessor authority before the lineage
            // queries below, which can be expensive on a large legacy store.
            return Ok(MigrationPageOutcome::UnmigratableLineage);
        }
        expected_sequence = expected_sequence.saturating_add(1);
    }
    let page_last_sequence_i64 = i64::try_from(page_last_sequence)
        .map_err(|_| storage_message("migrate projection frontier", "sequence overflow"))?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO observation_projection_aliases (
                projector_version, observation_id, output_provider, output_message_id
             )
             SELECT ?1, legacy.observation_id, legacy.output_provider, legacy.output_message_id
             FROM observation_projection_aliases AS legacy
             JOIN observations AS observation
               ON observation.observation_id = legacy.observation_id
             WHERE legacy.projector_version = ?2
               AND observation.sequence > ?3 AND observation.sequence <= ?4
               AND EXISTS (
                    SELECT 1 FROM observation_projection_provenance AS provenance
                    WHERE provenance.projector_version = ?2
                      AND provenance.observation_id = legacy.observation_id
               )",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                predecessor.version.as_str(),
                migrated_frontier_i64,
                page_last_sequence_i64
            ],
        )
        .await
        .map_err(|error| storage("copy predecessor projection aliases", error))?;
    transaction
        .execute(
            "WITH page_outputs AS (
                SELECT DISTINCT page.output_provider, page.output_message_id
                FROM observation_projection_provenance AS page
                JOIN observations AS page_observation
                  ON page_observation.observation_id = page.observation_id
                WHERE page.projector_version = ?2
                  AND page_observation.sequence > ?3
                  AND page_observation.sequence <= ?4
             ), latest_observations AS (
                SELECT DISTINCT latest.observation_id
                FROM observation_projection_provenance AS latest
                JOIN observations AS latest_observation
                  ON latest_observation.observation_id = latest.observation_id
                WHERE latest.projector_version = ?2
                  AND EXISTS (
                    SELECT 1 FROM page_outputs
                    WHERE page_outputs.output_provider = latest.output_provider
                      AND page_outputs.output_message_id = latest.output_message_id
                  )
                  AND NOT EXISTS (
                    SELECT 1
                    FROM observation_projection_provenance AS newer
                    JOIN observations AS newer_observation
                      ON newer_observation.observation_id = newer.observation_id
                    WHERE newer.projector_version = ?2
                      AND newer.output_provider = latest.output_provider
                      AND newer.output_message_id = latest.output_message_id
                      AND (newer_observation.sequence > latest_observation.sequence
                        OR (newer_observation.sequence = latest_observation.sequence
                          AND newer.observation_id > latest.observation_id))
                  )
             )
             INSERT OR IGNORE INTO observation_projection_aliases (
                projector_version, observation_id, output_provider, output_message_id
             )
             SELECT ?1, legacy.observation_id, legacy.output_provider, legacy.output_message_id
             FROM observation_projection_aliases AS legacy
             WHERE legacy.projector_version = ?2
               AND legacy.observation_id IN (SELECT observation_id FROM latest_observations)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                predecessor.version.as_str(),
                migrated_frontier_i64,
                page_last_sequence_i64
            ],
        )
        .await
        .map_err(|error| storage("copy predecessor lineage aliases", error))?;

    let mut seed_rows = transaction
        .query(
            "WITH page_outputs AS (
                SELECT DISTINCT page.output_provider, page.output_message_id
                FROM observation_projection_provenance AS page
                JOIN observations AS page_observation
                  ON page_observation.observation_id = page.observation_id
                WHERE page.projector_version = ?1
                  AND page_observation.sequence > ?2
                  AND page_observation.sequence <= ?3
             )
             SELECT DISTINCT latest_observation.sequence,
                    latest_observation.observation_json
             FROM observation_projection_provenance AS latest
             JOIN observations AS latest_observation
               ON latest_observation.observation_id = latest.observation_id
             WHERE latest.projector_version = ?1
               AND EXISTS (
                    SELECT 1 FROM page_outputs
                    WHERE page_outputs.output_provider = latest.output_provider
                      AND page_outputs.output_message_id = latest.output_message_id
               )
               AND NOT EXISTS (
                    SELECT 1
                    FROM observation_projection_provenance AS newer
                    JOIN observations AS newer_observation
                      ON newer_observation.observation_id = newer.observation_id
                    WHERE newer.projector_version = ?1
                      AND newer.output_provider = latest.output_provider
                      AND newer.output_message_id = latest.output_message_id
                      AND (newer_observation.sequence > latest_observation.sequence
                        OR (newer_observation.sequence = latest_observation.sequence
                          AND newer.observation_id > latest.observation_id))
               )
             ORDER BY latest_observation.sequence, latest.observation_id",
            params![
                predecessor.version.as_str(),
                migrated_frontier_i64,
                page_last_sequence_i64
            ],
        )
        .await
        .map_err(|error| storage("read predecessor projection lineage", error))?;
    let mut lineage_seeds = Vec::new();
    while let Some(row) = seed_rows
        .next()
        .await
        .map_err(|error| storage("read predecessor projection lineage", error))?
    {
        lineage_seeds.push(decode_observation_row(
            &row,
            "decode predecessor projection lineage",
        )?);
    }
    drop(seed_rows);
    for (sequence, observation) in lineage_seeds {
        seed_predecessor_message_lineage(
            transaction,
            sequence,
            &observation,
            predecessor.version.as_str(),
        )
        .await?;
        inherit_predecessor_output_state(
            transaction,
            observation.observation_id().as_str(),
            predecessor.version.as_str(),
        )
        .await?;
    }

    for ((sequence, observation), _) in page {
        let effect = derive_projection_with_alias(transaction, &observation).await?;
        apply_effect(transaction, sequence, &observation, &effect).await?;
        inherit_predecessor_output_state(
            transaction,
            observation.observation_id().as_str(),
            predecessor.version.as_str(),
        )
        .await?;
        consume_projection_queue_item(transaction, observation.observation_id()).await?;
        write_checkpoint(transaction, sequence).await?;
        migrated_frontier = sequence;
    }
    let completed = i64::from(migrated_frontier == predecessor.sequence);
    let advanced = transaction
        .execute(
            "UPDATE observation_projection_migrations
             SET migrated_through = ?3,
                 completed = ?4
             WHERE source_projector_version = ?1
               AND target_projector_version = ?2
               AND source_frontier = ?5
               AND migrated_through = ?6
               AND completed = 0",
            params![
                predecessor.version.as_str(),
                SESSION_MESSAGE_PROJECTOR_VERSION,
                i64::try_from(migrated_frontier).map_err(|_| storage_message(
                    "advance projection version migration",
                    "sequence overflow"
                ))?,
                completed,
                i64::try_from(predecessor.sequence).map_err(|_| storage_message(
                    "advance projection version migration",
                    "sequence overflow"
                ))?,
                i64::try_from(progress.migrated_through).map_err(|_| storage_message(
                    "advance projection version migration",
                    "sequence overflow"
                ))?
            ],
        )
        .await
        .map_err(|error| storage("advance projection version migration", error))?;
    if advanced != 1 {
        return Err(storage_message(
            "advance projection version migration",
            "migration watermark compare-and-swap failed",
        ));
    }
    Ok(MigrationPageOutcome::Advanced)
}

async fn projection_version_migration_pending(
    conn: &impl QueryExecutor,
) -> ProjectionStoreResult<bool> {
    let Some(predecessor) = read_predecessor_frontier(conn).await? else {
        return Ok(false);
    };
    Ok(read_migration_progress(conn, &predecessor)
        .await?
        .is_none_or(|progress| !progress.completed))
}

async fn read_migration_progress(
    conn: &impl QueryExecutor,
    predecessor: &PredecessorFrontier,
) -> ProjectionStoreResult<Option<MigrationProgress>> {
    let mut rows = conn
        .query(
            "SELECT source_frontier, migrated_through, completed
             FROM observation_projection_migrations
             WHERE source_projector_version = ?1
               AND target_projector_version = ?2",
            params![
                predecessor.version.as_str(),
                SESSION_MESSAGE_PROJECTOR_VERSION
            ],
        )
        .await
        .map_err(|error| storage("read projection version migration", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection version migration", error))?
    else {
        return Ok(None);
    };
    let source_frontier = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read projection version migration", error))?,
        "decode projection migration source frontier",
    )?;
    if source_frontier != predecessor.sequence {
        return Err(storage_message(
            "read projection version migration",
            "predecessor frontier changed after migration began",
        ));
    }
    let migrated_through = decode_sequence(
        row.get::<i64>(1)
            .map_err(|error| storage("read projection version migration", error))?,
        "decode projection migration progress",
    )?;
    let completed = row
        .get::<i64>(2)
        .map_err(|error| storage("read projection version migration", error))?
        != 0;
    drop(rows);
    if migrated_through > predecessor.sequence
        || completed != (migrated_through == predecessor.sequence)
    {
        return Err(storage_message(
            "read projection version migration",
            "migration progress is inconsistent with its source frontier",
        ));
    }
    Ok(Some(MigrationProgress {
        migrated_through,
        completed,
    }))
}

async fn read_predecessor_frontier(
    conn: &impl QueryExecutor,
) -> ProjectionStoreResult<Option<PredecessorFrontier>> {
    let mut rows = conn
        .query(
            "SELECT projector_version, last_sequence
             FROM observation_projection_checkpoints
             WHERE projector_version = ?1
                OR (projector_version = ?2 AND NOT EXISTS (
                    SELECT 1 FROM observation_projection_checkpoints
                    WHERE projector_version = ?1
                ))
                OR (projector_version = ?3 AND NOT EXISTS (
                    SELECT 1 FROM observation_projection_checkpoints
                    WHERE projector_version = ?1 OR projector_version = ?2
                ))
             ORDER BY CASE projector_version WHEN ?1 THEN 0 WHEN ?2 THEN 1 ELSE 2 END
             LIMIT 1",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V1
            ],
        )
        .await
        .map_err(|error| storage("read predecessor projection frontier", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read predecessor projection frontier", error))?
    else {
        return Ok(None);
    };
    let version = row
        .get::<String>(0)
        .map_err(|error| storage("read predecessor projection frontier", error))?;
    let sequence = row
        .get::<i64>(1)
        .map_err(|error| storage("read predecessor projection frontier", error))?;
    drop(rows);
    Ok(Some(PredecessorFrontier {
        version,
        sequence: decode_sequence(sequence, "decode predecessor projection frontier")?,
    }))
}

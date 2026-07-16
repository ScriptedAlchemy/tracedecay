use libsql::{Connection, params};
use tracedecay_store::{
    ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SESSION_MESSAGE_PROJECTOR_VERSION_V2, SESSION_MESSAGE_PROJECTOR_VERSION_V3,
};

use super::super::GlobalDb;
use super::apply::{apply_effect, derive_projection_with_alias, seed_predecessor_message_lineage};
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

pub(in crate::global_db) async fn prepare_projection_version_migration(
    db: &GlobalDb,
) -> ProjectionStoreResult<()> {
    if SESSION_MESSAGE_PROJECTOR_VERSION == SESSION_MESSAGE_PROJECTOR_VERSION_V1 {
        return Ok(());
    }
    if SESSION_MESSAGE_PROJECTOR_VERSION != SESSION_MESSAGE_PROJECTOR_VERSION_V3 {
        return Err(storage_message(
            "prepare projection version migration",
            "current projector version has no registered migration",
        ));
    }
    if !projection_version_migration_pending(&db.conn).await? {
        return Ok(());
    }

    migrate_projection_page(db).await?;
    Ok(())
}

pub(super) async fn migrate_projection_page(db: &GlobalDb) -> ProjectionStoreResult<bool> {
    let transaction = db
        .begin_write_transaction()
        .await
        .map_err(|error| storage("begin projection version migration page", error))?;

    let Some(predecessor) = read_predecessor_frontier(&transaction).await? else {
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit projection version migration page", error))?;
        return Ok(false);
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
    let progress = read_migration_progress(&transaction, &predecessor)
        .await?
        .ok_or_else(|| {
            storage_message(
                "read projection version migration",
                "initialized migration watermark disappeared",
            )
        })?;
    if progress.completed {
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit projection version migration page", error))?;
        return Ok(false);
    }

    ensure_projection_output_state_cache(&transaction).await?;
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
            &transaction,
            sequence,
            &observation,
            predecessor.version.as_str(),
        )
        .await?;
        inherit_predecessor_output_state(
            &transaction,
            observation.observation_id().as_str(),
            predecessor.version.as_str(),
        )
        .await?;
    }

    for ((sequence, observation), predecessor_outcomes) in page {
        if sequence != migrated_frontier.saturating_add(1) || predecessor_outcomes != 1 {
            return Err(storage_message(
                "migrate projection frontier",
                "predecessor frontier lacks exactly one terminal outcome",
            ));
        }
        let effect = derive_projection_with_alias(&transaction, &observation).await?;
        apply_effect(&transaction, sequence, &observation, &effect).await?;
        inherit_predecessor_output_state(
            &transaction,
            observation.observation_id().as_str(),
            predecessor.version.as_str(),
        )
        .await?;
        consume_projection_queue_item(&transaction, observation.observation_id()).await?;
        write_checkpoint(&transaction, sequence).await?;
        migrated_frontier = sequence;
    }
    let completed = i64::from(migrated_frontier == predecessor.sequence);
    let advanced = transaction
        .execute(
            "UPDATE observation_projection_migrations
             SET migrated_through = ?3,
                 completed = ?4
             WHERE source_projector_version = ?1
               AND target_projector_version = ?2",
            params![
                predecessor.version.as_str(),
                SESSION_MESSAGE_PROJECTOR_VERSION,
                i64::try_from(migrated_frontier).map_err(|_| storage_message(
                    "advance projection version migration",
                    "sequence overflow"
                ))?,
                completed
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
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection version migration page", error))?;
    Ok(migrated_frontier < predecessor.sequence)
}

async fn projection_version_migration_pending(conn: &Connection) -> ProjectionStoreResult<bool> {
    let Some(predecessor) = read_predecessor_frontier(conn).await? else {
        return Ok(false);
    };
    Ok(read_migration_progress(conn, &predecessor)
        .await?
        .is_none_or(|progress| !progress.completed))
}

async fn read_migration_progress(
    conn: &Connection,
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
    Ok(Some(MigrationProgress {
        migrated_through,
        completed,
    }))
}

async fn read_predecessor_frontier(
    conn: &Connection,
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
             ORDER BY CASE projector_version WHEN ?1 THEN 0 ELSE 1 END
             LIMIT 1",
            params![
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

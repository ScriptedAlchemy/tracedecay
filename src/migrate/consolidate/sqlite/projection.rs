use libsql::Connection;

use super::{db_error, db_message, query_i64, quote_identifier};
use crate::errors::Result;

fn state_ctes(target: &str, target_messages: &str, source: &str) -> String {
    let target = quote_identifier(target);
    let target_messages = quote_identifier(target_messages);
    let source = quote_identifier(source);
    format!(
        "WITH source_mapped_provenance AS (
             SELECT p.projector_version, p.observation_id, p.receipt_id,
                    p.output_provider, p.output_message_id, m.mapped_id,
                    p.output_digest, p.message_created
             FROM {source}.observation_projection_provenance AS p
             JOIN consolidation_message_map AS m
               ON m.provider=p.output_provider AND m.original_id=p.output_message_id
         ), source_alias_claims AS (
             SELECT a.projector_version, a.observation_id, a.output_provider,
                    COALESCE(m.mapped_id, a.output_message_id) AS output_message_id
             FROM {source}.observation_projection_aliases AS a
             LEFT JOIN consolidation_message_map AS m
               ON m.provider=a.output_provider AND m.original_id=a.output_message_id
             WHERE NOT EXISTS (
                 SELECT 1 FROM {target_messages}.observations AS stable_target
                 WHERE stable_target.observation_id=a.observation_id
             )
             UNION ALL
             SELECT projector_version, observation_id, output_provider, mapped_id
             FROM source_mapped_provenance
             WHERE NOT EXISTS (
                 SELECT 1 FROM {target_messages}.observations AS stable_target
                 WHERE stable_target.observation_id=source_mapped_provenance.observation_id
             )
         ), source_aliases AS (
             SELECT DISTINCT projector_version, observation_id,
                    output_provider, output_message_id
             FROM source_alias_claims
         ), displaced_target_provenance AS (
             SELECT p.projector_version, p.observation_id, p.receipt_id,
                    p.output_provider, p.output_message_id,
                    p.output_digest, p.message_created
             FROM {target}.observation_projection_provenance AS p
             JOIN source_aliases AS a
               ON a.projector_version=p.projector_version
              AND a.observation_id=p.observation_id
             WHERE a.output_provider IS NOT p.output_provider
                OR a.output_message_id IS NOT p.output_message_id
         ), retained_target_provenance AS (
             SELECT p.projector_version, p.observation_id, p.receipt_id,
                    p.output_provider, p.output_message_id,
                    p.output_digest, p.message_created
             FROM {target}.observation_projection_provenance AS p
             WHERE NOT EXISTS (
                 SELECT 1 FROM displaced_target_provenance AS d
                 WHERE d.projector_version=p.projector_version
                   AND d.observation_id=p.observation_id
             )
         ), source_retained_provenance AS (
             SELECT p.projector_version, p.observation_id, p.receipt_id,
                    p.output_provider, p.output_message_id, p.output_digest,
                    CASE WHEN EXISTS (
                         SELECT 1 FROM {target_messages}.session_messages AS message
                         WHERE message.provider=p.output_provider
                           AND message.message_id=p.output_message_id
                    ) AND NOT EXISTS (
                         SELECT 1
                         FROM {target}.observation_projection_provenance AS owner
                         WHERE owner.projector_version=p.projector_version
                           AND owner.output_provider=p.output_provider
                           AND owner.output_message_id=p.output_message_id
                           AND owner.message_created=1
                    ) THEN 0 ELSE p.message_created END AS message_created
             FROM {source}.observation_projection_provenance AS p
             LEFT JOIN consolidation_message_map AS m
               ON m.provider=p.output_provider AND m.original_id=p.output_message_id
             WHERE m.mapped_id IS NULL
         ), expected_provenance_claims AS (
             SELECT * FROM retained_target_provenance
             UNION ALL
             SELECT * FROM source_retained_provenance
         ), expected_provenance AS (
             SELECT projector_version, observation_id, MIN(receipt_id) AS receipt_id,
                    MIN(output_provider) AS output_provider,
                    MIN(output_message_id) AS output_message_id,
                    MIN(output_digest) AS output_digest,
                    MAX(message_created) AS message_created
             FROM expected_provenance_claims
             GROUP BY projector_version, observation_id
         ), expected_aliases AS (
             SELECT a.projector_version, a.observation_id,
                    a.output_provider, a.output_message_id
             FROM {target}.observation_projection_aliases AS a
             WHERE NOT EXISTS (
                 SELECT 1 FROM source_aliases AS s
                 WHERE s.projector_version=a.projector_version
                   AND s.observation_id=a.observation_id
             )
             UNION
             SELECT projector_version, observation_id,
                    output_provider, output_message_id
             FROM source_aliases
         ), retained_outputs AS (
             SELECT DISTINCT output_provider, output_message_id
             FROM expected_provenance
         ), source_owned_mapped_outputs AS (
             SELECT output_provider, mapped_id AS output_message_id
             FROM source_mapped_provenance
             GROUP BY output_provider, mapped_id
             HAVING MAX(message_created)=1
         ), displaced_owned_outputs AS (
             SELECT output_provider, output_message_id
             FROM displaced_target_provenance
             GROUP BY output_provider, output_message_id
             HAVING MAX(message_created)=1
         ), removed_projection_outputs AS (
             SELECT output_provider, output_message_id
             FROM source_owned_mapped_outputs AS owned
             WHERE NOT EXISTS (
                 SELECT 1 FROM retained_outputs AS retained
                 WHERE retained.output_provider=owned.output_provider
                   AND retained.output_message_id=owned.output_message_id
             )
             UNION
             SELECT output_provider, output_message_id
             FROM displaced_owned_outputs AS owned
             WHERE NOT EXISTS (
                 SELECT 1 FROM retained_outputs AS retained
                 WHERE retained.output_provider=owned.output_provider
                   AND retained.output_message_id=owned.output_message_id
             )
         )"
    )
}

fn with_state(target: &str, target_messages: &str, source: &str, sql: &str) -> String {
    format!("{} {sql}", state_ctes(target, target_messages, source))
}

pub(super) async fn preflight(conn: &Connection) -> Result<()> {
    let alias_conflicts = query_i64(
        conn,
        &with_state(
            "main",
            "target_input",
            "source",
            "SELECT COUNT(*) FROM (
                 SELECT projector_version, observation_id
                 FROM source_alias_claims
                 GROUP BY projector_version, observation_id
                 HAVING MIN(output_provider) IS NOT MAX(output_provider)
                     OR MIN(output_message_id) IS NOT MAX(output_message_id)
             )",
        ),
    )
    .await?;
    if alias_conflicts != 0 {
        return Err(db_message(
            "merge_observation_authority",
            "projection output collision cannot be represented by one durable alias",
        ));
    }

    let provenance_conflicts = query_i64(
        conn,
        &with_state(
            "main",
            "target_input",
            "source",
            "SELECT COUNT(*)
             FROM retained_target_provenance AS t
             JOIN source_retained_provenance AS s
               ON s.projector_version=t.projector_version
              AND s.observation_id=t.observation_id
             WHERE t.receipt_id IS NOT s.receipt_id
                OR t.output_provider IS NOT s.output_provider
                OR t.output_message_id IS NOT s.output_message_id
                OR t.output_digest IS NOT s.output_digest",
        ),
    )
    .await?;
    if provenance_conflicts != 0 {
        return Err(db_message(
            "merge_observation_authority",
            "projection provenance collision cannot be represented losslessly",
        ));
    }

    let disposition_conflicts = query_i64(
        conn,
        "SELECT COUNT(*)
         FROM main.observation_projection_dispositions AS t
         JOIN source.observation_projection_dispositions AS s
           ON s.projector_version=t.projector_version
          AND s.observation_id=t.observation_id
         WHERE t.receipt_id IS NOT s.receipt_id OR t.reason IS NOT s.reason",
    )
    .await?;
    if disposition_conflicts != 0 {
        return Err(db_message(
            "merge_observation_authority",
            "projection disposition collision cannot be represented losslessly",
        ));
    }
    Ok(())
}

pub(super) async fn merge(conn: &Connection) -> Result<()> {
    for sql in [
        with_state(
            "main",
            "target_input",
            "source",
            "INSERT INTO observation_projection_aliases(
                 projector_version, observation_id, output_provider, output_message_id
             )
             SELECT projector_version, observation_id, output_provider, output_message_id
             FROM source_aliases WHERE 1
             ON CONFLICT(projector_version, observation_id) DO UPDATE SET
                 output_provider=excluded.output_provider,
                 output_message_id=excluded.output_message_id",
        ),
        with_state(
            "main",
            "target_input",
            "source",
            "DELETE FROM session_messages AS message
             WHERE EXISTS (
                 SELECT 1 FROM removed_projection_outputs AS removed
                 WHERE removed.output_provider=message.provider
                   AND removed.output_message_id=message.message_id
             )",
        ),
        with_state(
            "main",
            "target_input",
            "source",
            "DELETE FROM observation_projection_provenance AS provenance
             WHERE EXISTS (
                 SELECT 1 FROM displaced_target_provenance AS displaced
                 WHERE displaced.projector_version=provenance.projector_version
                   AND displaced.observation_id=provenance.observation_id
             )",
        ),
        with_state(
            "main",
            "target_input",
            "source",
            "INSERT INTO observation_projection_provenance(
                 projector_version, observation_id, receipt_id, output_provider,
                 output_message_id, output_digest, message_created
             )
             SELECT projector_version, observation_id, receipt_id, output_provider,
                    output_message_id, output_digest, message_created
             FROM source_retained_provenance WHERE 1
             ON CONFLICT(projector_version, observation_id) DO UPDATE SET
                 message_created=MAX(
                     observation_projection_provenance.message_created,
                     excluded.message_created
                 )",
        ),
    ] {
        conn.execute_batch(&sql)
            .await
            .map_err(|error| db_error("merge_projection_state", error))?;
    }
    conn.execute_batch(
        "INSERT OR IGNORE INTO observation_projection_dispositions(
             projector_version, observation_id, receipt_id, reason
         )
         SELECT projector_version, observation_id, receipt_id, reason
         FROM source.observation_projection_dispositions;
         DELETE FROM observation_projection_checkpoints;
         DELETE FROM projection_queue;
         INSERT INTO projection_queue(observation_id, observation_sequence)
         SELECT observation_id, sequence FROM observations ORDER BY sequence;",
    )
    .await
    .map_err(|error| db_error("merge_projection_state", error))?;
    Ok(())
}

pub(super) fn expected_session_messages(session_metadata: &str) -> String {
    with_state(
        "target_input",
        "target_input",
        "source_input",
        &format!(
            "SELECT t.provider, t.message_id, t.session_id, t.role, t.timestamp,
                    t.ordinal, t.text, t.kind, t.model, t.tool_names, t.source_path,
                    t.source_offset, t.metadata_json
             FROM target_input.session_messages AS t
             WHERE NOT EXISTS (
                 SELECT 1 FROM removed_projection_outputs AS removed
                 WHERE removed.output_provider=t.provider
                   AND removed.output_message_id=t.message_id
             )
             UNION ALL
             SELECT s.provider, COALESCE(m.mapped_id, s.message_id), s.session_id,
                    s.role, s.timestamp, s.ordinal, s.text, s.kind, s.model,
                    s.tool_names, s.source_path, s.source_offset, {session_metadata}
             FROM source_input.session_messages AS s
             LEFT JOIN consolidation_message_map AS m
               ON m.provider=s.provider AND m.original_id=s.message_id
             WHERE (m.mapped_id IS NOT NULL OR NOT EXISTS (
                 SELECT 1 FROM target_input.session_messages AS t
                 WHERE t.provider=s.provider AND t.message_id=s.message_id
             ))
               AND NOT EXISTS (
                 SELECT 1 FROM removed_projection_outputs AS removed
                 WHERE removed.output_provider=s.provider
                   AND removed.output_message_id=COALESCE(m.mapped_id, s.message_id)
             )"
        ),
    )
}

pub(super) async fn verify(conn: &Connection) -> Result<()> {
    for (label, table, columns, expected) in [
        (
            "projection aliases",
            "observation_projection_aliases",
            "projector_version, observation_id, output_provider, output_message_id",
            "SELECT projector_version, observation_id, output_provider, output_message_id
             FROM expected_aliases",
        ),
        (
            "projection provenance",
            "observation_projection_provenance",
            "projector_version, observation_id, receipt_id, output_provider,
             output_message_id, output_digest, message_created",
            "SELECT projector_version, observation_id, receipt_id, output_provider,
                    output_message_id, output_digest, message_created
             FROM expected_provenance",
        ),
    ] {
        let differences = query_i64(
            conn,
            &with_state(
                "target_input",
                "target_input",
                "source_input",
                &format!(
                    "SELECT
                       (SELECT COUNT(*) FROM (
                            {expected} EXCEPT SELECT {columns} FROM main.{table}
                        ))
                     + (SELECT COUNT(*) FROM (
                            SELECT {columns} FROM main.{table} EXCEPT {expected}
                        ))"
                ),
            ),
        )
        .await?;
        if differences != 0 {
            return Err(db_message(
                "verify_consolidation",
                format!("destination {label} differs from canonical projection state"),
            ));
        }
    }

    let disposition_differences = query_i64(
        conn,
        "WITH expected AS (
             SELECT projector_version, observation_id, receipt_id, reason
             FROM target_input.observation_projection_dispositions
             UNION
             SELECT projector_version, observation_id, receipt_id, reason
             FROM source_input.observation_projection_dispositions
         )
         SELECT
           (SELECT COUNT(*) FROM (
                SELECT * FROM expected EXCEPT
                SELECT projector_version, observation_id, receipt_id, reason
                FROM main.observation_projection_dispositions
            ))
         + (SELECT COUNT(*) FROM (
                SELECT projector_version, observation_id, receipt_id, reason
                FROM main.observation_projection_dispositions
                EXCEPT SELECT * FROM expected
            ))",
    )
    .await?;
    if disposition_differences != 0 {
        return Err(db_message(
            "verify_consolidation",
            "destination projection dispositions differ from frozen inputs",
        ));
    }

    let orphaned = query_i64(
        conn,
        "SELECT COUNT(*)
         FROM observation_projection_provenance AS provenance
         LEFT JOIN session_messages AS message
           ON message.provider=provenance.output_provider
          AND message.message_id=provenance.output_message_id
         WHERE message.message_id IS NULL",
    )
    .await?;
    if orphaned != 0 {
        return Err(db_message(
            "verify_consolidation",
            "destination contains orphaned projection provenance",
        ));
    }
    Ok(())
}

use tracedecay_runtime_core::db::engine::Executor;

use super::{db_error, db_message, query_i64, quote_identifier};
use tracedecay_runtime_core::errors::Result;

const PLAN_TABLES: &[&str] = &[
    "consolidation_projection_stable_claims",
    "consolidation_projection_source_mapped",
    "consolidation_projection_alias_claims",
    "consolidation_projection_alias_plan",
    "consolidation_projection_provenance_candidates",
    "consolidation_projection_provenance_claims",
    "consolidation_projection_provenance_plan",
    "consolidation_projection_displaced",
    "consolidation_projection_removal_plan",
    "consolidation_projection_disposition_claims",
    "consolidation_projection_disposition_plan",
];

pub(super) async fn materialize(conn: &impl Executor, target: &str, source: &str) -> Result<()> {
    for table in PLAN_TABLES {
        conn.execute(&format!("DROP TABLE IF EXISTS temp.{table}"), ())
            .await
            .map_err(|error| db_error("materialize_projection_plan", error))?;
    }
    let target = quote_identifier(target);
    let source = quote_identifier(source);
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE consolidation_projection_stable_claims(
             projector_version TEXT NOT NULL,
             observation_id TEXT NOT NULL,
             PRIMARY KEY(projector_version, observation_id)
         );
         INSERT INTO consolidation_projection_stable_claims
         SELECT projector_version, observation_id
         FROM {target}.observation_projection_aliases
         UNION
         SELECT projector_version, observation_id
         FROM {target}.observation_projection_provenance;

         CREATE TEMP TABLE consolidation_projection_source_mapped AS
         SELECT p.projector_version, p.observation_id, p.output_ordinal, p.receipt_id,
                p.output_provider, p.output_message_id, m.mapped_id,
                p.output_digest, p.message_created
         FROM {source}.observation_projection_provenance AS p
         JOIN consolidation_message_map AS m
           ON m.provider=p.output_provider AND m.original_id=p.output_message_id;

         CREATE TEMP TABLE consolidation_projection_alias_claims AS
         SELECT a.projector_version, a.observation_id, a.output_provider,
                COALESCE(m.mapped_id, a.output_message_id) AS output_message_id
         FROM {source}.observation_projection_aliases AS a
         LEFT JOIN consolidation_message_map AS m
           ON m.provider=a.output_provider AND m.original_id=a.output_message_id
         WHERE NOT EXISTS (
             SELECT 1 FROM consolidation_projection_stable_claims AS stable
             WHERE stable.projector_version=a.projector_version
               AND stable.observation_id=a.observation_id
         )
         UNION ALL
         SELECT mapped.projector_version, mapped.observation_id,
                mapped.output_provider, mapped.mapped_id
         FROM consolidation_projection_source_mapped AS mapped
         WHERE mapped.output_ordinal=0
           AND NOT EXISTS (
             SELECT 1 FROM consolidation_projection_stable_claims AS stable
             WHERE stable.projector_version=mapped.projector_version
               AND stable.observation_id=mapped.observation_id
         );

         CREATE TEMP TABLE consolidation_projection_alias_plan(
             projector_version TEXT NOT NULL,
             observation_id TEXT NOT NULL,
             output_provider TEXT NOT NULL,
             output_message_id TEXT NOT NULL,
             PRIMARY KEY(projector_version, observation_id)
         );
         INSERT INTO consolidation_projection_alias_plan
         SELECT projector_version, observation_id,
                MIN(output_provider), MIN(output_message_id)
         FROM (
             SELECT projector_version, observation_id,
                    output_provider, output_message_id
             FROM {target}.observation_projection_aliases
             UNION ALL
             SELECT projector_version, observation_id,
                    output_provider, output_message_id
             FROM consolidation_projection_alias_claims
         )
         GROUP BY projector_version, observation_id;

         CREATE TEMP TABLE consolidation_projection_provenance_candidates AS
         SELECT p.projector_version, p.observation_id, p.output_ordinal, p.receipt_id,
                p.output_provider, p.output_message_id,
                p.output_digest, p.message_created, p.retrieval_anchor_id
         FROM {target}.observation_projection_provenance AS p
         UNION ALL
         SELECT p.projector_version, p.observation_id, p.output_ordinal, p.receipt_id,
                p.output_provider, p.output_message_id, p.output_digest,
                CASE WHEN EXISTS (
                     SELECT 1 FROM {target}.session_messages AS message
                     WHERE message.provider=p.output_provider
                       AND message.message_id=p.output_message_id
                ) AND NOT EXISTS (
                     SELECT 1
                     FROM {target}.observation_projection_provenance AS owner
                     WHERE owner.projector_version=p.projector_version
                       AND owner.output_provider=p.output_provider
                       AND owner.output_message_id=p.output_message_id
                       AND owner.message_created=1
                ) THEN 0 ELSE p.message_created END,
                p.retrieval_anchor_id
         FROM {source}.observation_projection_provenance AS p
         LEFT JOIN consolidation_message_map AS m
           ON m.provider=p.output_provider AND m.original_id=p.output_message_id
         WHERE m.mapped_id IS NULL;

         CREATE TEMP TABLE consolidation_projection_displaced AS
         SELECT p.projector_version, p.observation_id, p.output_ordinal, p.receipt_id,
                p.output_provider, p.output_message_id,
                p.output_digest, p.message_created
         FROM consolidation_projection_provenance_candidates AS p
         JOIN consolidation_projection_alias_plan AS alias
           ON alias.projector_version=p.projector_version
          AND alias.observation_id=p.observation_id
         WHERE p.output_ordinal=0
           AND (alias.output_provider IS NOT p.output_provider
                OR alias.output_message_id IS NOT p.output_message_id);

         CREATE TEMP TABLE consolidation_projection_provenance_claims AS
         SELECT p.*
         FROM consolidation_projection_provenance_candidates AS p
         WHERE NOT EXISTS (
             SELECT 1 FROM consolidation_projection_displaced AS displaced
             WHERE displaced.projector_version=p.projector_version
               AND displaced.observation_id=p.observation_id
               AND displaced.output_ordinal=p.output_ordinal
         );

         CREATE TEMP TABLE consolidation_projection_provenance_plan(
             projector_version TEXT NOT NULL,
             observation_id TEXT NOT NULL,
             output_ordinal INTEGER NOT NULL,
             receipt_id TEXT NOT NULL,
             output_provider TEXT NOT NULL,
             output_message_id TEXT NOT NULL,
             output_digest TEXT NOT NULL,
             message_created INTEGER NOT NULL,
             retrieval_anchor_id TEXT,
             PRIMARY KEY(projector_version, observation_id, output_ordinal)
         );
         INSERT INTO consolidation_projection_provenance_plan
         SELECT projector_version, observation_id, output_ordinal, MIN(receipt_id),
                MIN(output_provider), MIN(output_message_id), MIN(output_digest),
                MAX(message_created), MIN(retrieval_anchor_id)
         FROM consolidation_projection_provenance_claims
         GROUP BY projector_version, observation_id, output_ordinal;

         CREATE TEMP TABLE consolidation_projection_removal_plan(
             output_provider TEXT NOT NULL,
             output_message_id TEXT NOT NULL,
             PRIMARY KEY(output_provider, output_message_id)
         );
         INSERT INTO consolidation_projection_removal_plan
         WITH retained AS (
             SELECT DISTINCT output_provider, output_message_id
             FROM consolidation_projection_provenance_plan
         ), owned AS (
             SELECT output_provider, mapped_id AS output_message_id
             FROM consolidation_projection_source_mapped
             GROUP BY output_provider, mapped_id
             HAVING MAX(message_created)=1
             UNION
             SELECT output_provider, output_message_id
             FROM consolidation_projection_displaced
             GROUP BY output_provider, output_message_id
             HAVING MAX(message_created)=1
         )
         SELECT output_provider, output_message_id FROM owned
         WHERE NOT EXISTS (
             SELECT 1 FROM retained
             WHERE retained.output_provider=owned.output_provider
               AND retained.output_message_id=owned.output_message_id
         );

         CREATE TEMP TABLE consolidation_projection_disposition_plan(
             projector_version TEXT NOT NULL,
             observation_id TEXT NOT NULL,
             receipt_id TEXT NOT NULL,
             reason TEXT NOT NULL,
             PRIMARY KEY(projector_version, observation_id)
         );
         CREATE TEMP TABLE consolidation_projection_disposition_claims AS
         SELECT projector_version, observation_id, receipt_id, reason
         FROM {target}.observation_projection_dispositions
         UNION ALL
         SELECT projector_version, observation_id, receipt_id, reason
         FROM {source}.observation_projection_dispositions;
         INSERT INTO consolidation_projection_disposition_plan
         SELECT projector_version, observation_id, MIN(receipt_id), MIN(reason)
         FROM consolidation_projection_disposition_claims
         GROUP BY projector_version, observation_id;"
    ))
    .await
    .map_err(|error| db_error("materialize_projection_plan", error))?;
    Ok(())
}

async fn validate_claims(conn: &impl Executor, operation: &'static str) -> Result<()> {
    for (query, message) in [
        (
            "SELECT COUNT(*) FROM (
                 SELECT projector_version, observation_id
                 FROM consolidation_projection_alias_claims
                 GROUP BY projector_version, observation_id
                 HAVING MIN(output_provider) IS NOT MAX(output_provider)
                     OR MIN(output_message_id) IS NOT MAX(output_message_id)
             )",
            "projection output collision cannot be represented by one durable alias",
        ),
        (
            "SELECT COUNT(*) FROM (
                 SELECT projector_version, observation_id, output_ordinal
                 FROM consolidation_projection_provenance_claims
                 GROUP BY projector_version, observation_id, output_ordinal
                 HAVING MIN(receipt_id) IS NOT MAX(receipt_id)
                     OR MIN(output_provider) IS NOT MAX(output_provider)
                     OR MIN(output_message_id) IS NOT MAX(output_message_id)
                     OR MIN(output_digest) IS NOT MAX(output_digest)
                     OR MIN(message_created) IS NOT MAX(message_created)
                     OR MIN(retrieval_anchor_id) IS NOT MAX(retrieval_anchor_id)
             )",
            "projection provenance collision cannot be represented losslessly",
        ),
        (
            "SELECT COUNT(*) FROM (
                 SELECT projector_version, observation_id
                 FROM consolidation_projection_disposition_claims
                 GROUP BY projector_version, observation_id
                 HAVING MIN(receipt_id) IS NOT MAX(receipt_id)
                     OR MIN(reason) IS NOT MAX(reason)
             )",
            "projection disposition collision cannot be represented losslessly",
        ),
    ] {
        if query_i64(conn, query).await? != 0 {
            return Err(db_message(operation, message));
        }
    }
    Ok(())
}

pub(super) async fn preflight(conn: &impl Executor) -> Result<()> {
    validate_claims(conn, "merge_observation_authority").await
}

pub(super) async fn merge(conn: &impl Executor) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM session_messages AS message
         WHERE EXISTS (
             SELECT 1 FROM consolidation_projection_removal_plan AS removed
             WHERE removed.output_provider=message.provider
               AND removed.output_message_id=message.message_id
         );
         DELETE FROM observation_projection_aliases;
         INSERT INTO observation_projection_aliases
         SELECT * FROM consolidation_projection_alias_plan;
         DELETE FROM observation_projection_provenance;
         INSERT INTO observation_projection_provenance
         SELECT * FROM consolidation_projection_provenance_plan;
         DELETE FROM observation_projection_dispositions;
         INSERT INTO observation_projection_dispositions
         SELECT * FROM consolidation_projection_disposition_plan;
         DELETE FROM observation_projection_rebuild_aliases;
         DELETE FROM observation_projection_rebuild_sessions;
         DELETE FROM observation_projection_rebuild_messages;
         DELETE FROM observation_projection_rebuild_provenance;
         DELETE FROM observation_projection_rebuild_dispositions;
         DELETE FROM observation_projection_rebuild_workflow_facts;
         DELETE FROM observation_projection_rebuilds;
         DELETE FROM observation_workflow_facts;
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
    format!(
        "SELECT t.provider, t.message_id, t.session_id, t.role, t.timestamp,
                t.ordinal, t.text, t.kind, t.model, t.tool_names, t.source_path,
                t.source_offset, t.metadata_json
         FROM target_input.session_messages AS t
         WHERE NOT EXISTS (
             SELECT 1 FROM consolidation_projection_removal_plan AS removed
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
             SELECT 1 FROM consolidation_projection_removal_plan AS removed
             WHERE removed.output_provider=s.provider
               AND removed.output_message_id=COALESCE(m.mapped_id, s.message_id)
         )"
    )
}

pub(super) async fn verify(conn: &impl Executor) -> Result<()> {
    validate_claims(conn, "verify_consolidation").await?;
    for (label, table, plan, columns) in [
        (
            "projection aliases",
            "observation_projection_aliases",
            "consolidation_projection_alias_plan",
            "projector_version, observation_id, output_provider, output_message_id",
        ),
        (
            "projection provenance",
            "observation_projection_provenance",
            "consolidation_projection_provenance_plan",
            "projector_version, observation_id, output_ordinal, receipt_id, output_provider,
             output_message_id, output_digest, message_created, retrieval_anchor_id",
        ),
        (
            "projection dispositions",
            "observation_projection_dispositions",
            "consolidation_projection_disposition_plan",
            "projector_version, observation_id, receipt_id, reason",
        ),
    ] {
        let differences = query_i64(
            conn,
            &format!(
                "SELECT
                   (SELECT COUNT(*) FROM (
                        SELECT {columns} FROM {plan}
                        EXCEPT SELECT {columns} FROM main.{table}
                    ))
                 + (SELECT COUNT(*) FROM (
                        SELECT {columns} FROM main.{table}
                        EXCEPT SELECT {columns} FROM {plan}
                    ))"
            ),
        )
        .await?;
        if differences != 0 {
            return Err(db_message(
                "verify_consolidation",
                format!("destination {label} differs from canonical projection plan"),
            ));
        }
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

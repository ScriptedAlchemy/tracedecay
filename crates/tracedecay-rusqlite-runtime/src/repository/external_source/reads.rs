use super::*;

#[hotpath::measure(label = "rusqlite.external_source.load_pending")]
pub(super) fn load_next_pending_projection(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
) -> rusqlite::Result<Option<SourcePendingProjectionV1>> {
    let pending = connection
        .prepare_cached(
            "SELECT pending.predecessor_frontier_digest,
                    pending.source_receipt_digest,
                    pending.successor_frontier_digest,
                    states.latest_projection_receipt_digest
             FROM external_source_states_v1 AS states
             JOIN external_source_pending_projections_v1 AS pending
               ON pending.binding_id = states.binding_id
              AND pending.predecessor_frontier_digest =
                  COALESCE(states.projection_frontier_digest, 'root')
             WHERE states.binding_id = ?1",
        )?
        .query_row(params![binding.binding_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .optional()?;
    let Some((
        predecessor_frontier_digest,
        receipt_digest,
        successor_frontier_digest,
        projection_receipt_digest,
    )) = pending
    else {
        return Ok(None);
    };
    let receipt = load_commit_receipt_by_digest(connection, binding, &receipt_digest)?
        .ok_or_else(|| invalid("external source pending receipt is missing"))?;
    if receipt.source_frontier().digest().as_str() != successor_frontier_digest {
        return Err(invalid(
            "external source pending successor frontier does not match its receipt",
        ));
    }
    let definition = load_definition(
        connection,
        binding.source_id.as_str(),
        i64::try_from(receipt.definition_revision())
            .map_err(|_| invalid("external source definition revision exceeds SQLite INTEGER"))?,
    )?;
    let source_binding = load_binding(
        connection,
        binding.binding_id.as_str(),
        i64::try_from(receipt.binding_revision())
            .map_err(|_| invalid("external source binding revision exceeds SQLite INTEGER"))?,
    )?;
    if source_binding.immutable_identity().map_err(invalid)? != *binding {
        return Err(invalid(
            "external source pending receipt does not match its binding key",
        ));
    }
    let expected_projection_frontier = projection_receipt_digest
        .as_deref()
        .map(|digest| {
            load_projection_receipt_by_digest(connection, binding, digest)?
                .ok_or_else(|| invalid("external source current projection receipt is missing"))
        })
        .transpose()?
        .map(|projection| projection.source_frontier().clone());
    if predecessor_frontier_digest != frontier_key(expected_projection_frontier.as_ref()) {
        return Err(invalid(
            "external source current projection frontier does not match its receipt",
        ));
    }
    let projected_mutations = if definition.deletion_semantics
        == SourceDeletionSemanticsV1::CompleteSnapshotAbsence
        && receipt.snapshot_completion().is_some()
    {
        load_current_mutations(
            connection,
            "external_source_projected_objects_v1",
            binding.binding_id.as_str(),
        )?
    } else {
        Vec::new()
    };
    SourcePendingProjectionV1::new(
        definition,
        source_binding,
        receipt,
        expected_projection_frontier,
        projected_mutations,
    )
    .map(Some)
    .map_err(invalid)
}

#[hotpath::measure(label = "rusqlite.external_source.load_pending_any")]
pub(super) fn load_next_pending_projection_any(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<Option<SourcePendingProjectionV1>> {
    let binding = connection
        .prepare_cached(
            "SELECT revisions.binding_json
             FROM external_source_pending_projections_v1 AS pending
             JOIN external_source_states_v1 AS states
               ON states.binding_id = pending.binding_id
              AND pending.predecessor_frontier_digest =
                  COALESCE(states.projection_frontier_digest, 'root')
             JOIN external_source_binding_revisions_v1 AS revisions
               ON revisions.binding_id = states.binding_id
              AND revisions.binding_revision = states.binding_revision
             ORDER BY pending.successor_sequence, pending.binding_id
             LIMIT 1",
        )?
        .query_row([], |row| {
            decode::<tracedecay_domain::SourceBindingV1>(row.get(0)?)
        })
        .optional()?;
    let Some(binding) = binding else {
        return Ok(None);
    };
    let identity = binding.immutable_identity().map_err(invalid)?;
    load_next_pending_projection(connection, &identity)
}

#[hotpath::measure(label = "rusqlite.external_source.load_commit_receipt_by_idempotency")]
pub(super) fn load_commit_receipt_by_idempotency(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
    key: &tracedecay_domain::ManifestDigest,
) -> rusqlite::Result<Option<SourceCommitReceiptV1>> {
    load_encoded_optional(
        connection,
        "SELECT receipt_json FROM external_source_commit_receipts_v1
         WHERE binding_id = ?1 AND idempotency_key = ?2",
        binding.binding_id.as_str(),
        key.as_str(),
    )
}

#[hotpath::measure(label = "rusqlite.external_source.load_commit_receipt_by_digest")]
pub(super) fn load_commit_receipt_by_digest(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
    digest: &str,
) -> rusqlite::Result<Option<SourceCommitReceiptV1>> {
    load_encoded_optional(
        connection,
        "SELECT receipt_json FROM external_source_commit_receipts_v1
         WHERE binding_id = ?1 AND receipt_digest = ?2",
        binding.binding_id.as_str(),
        digest,
    )
}

#[hotpath::measure(label = "rusqlite.external_source.load_authority_receipt")]
pub(super) fn load_authority_receipt(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
    key: &tracedecay_domain::ManifestDigest,
) -> rusqlite::Result<Option<SourceAuthorityPublicationReceiptV1>> {
    load_encoded_optional(
        connection,
        "SELECT receipt_json FROM external_source_authority_receipts_v1
         WHERE binding_id = ?1 AND idempotency_key = ?2",
        binding.binding_id.as_str(),
        key.as_str(),
    )
}

#[hotpath::measure(label = "rusqlite.external_source.load_projection_receipt")]
pub(super) fn load_projection_receipt(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
    digest: &tracedecay_domain::ManifestDigest,
) -> rusqlite::Result<Option<SourceProjectionCommitV1>> {
    load_projection_receipt_by_digest(connection, binding, digest.as_str())
}

#[hotpath::measure(label = "rusqlite.external_source.load_projection_receipt_by_digest")]
pub(super) fn load_projection_receipt_by_digest(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
    digest: &str,
) -> rusqlite::Result<Option<SourceProjectionCommitV1>> {
    load_encoded_optional(
        connection,
        "SELECT receipt_json FROM external_source_projection_publications_v1
         WHERE binding_id = ?1 AND projection_digest = ?2",
        binding.binding_id.as_str(),
        digest,
    )
}

#[hotpath::measure(label = "rusqlite.external_source.load_encoded_optional")]
fn load_encoded_optional<T: serde::de::DeserializeOwned>(
    connection: &rusqlite::Connection,
    sql: &str,
    binding_id: &str,
    key: &str,
) -> rusqlite::Result<Option<T>> {
    connection
        .prepare_cached(sql)?
        .query_row(params![binding_id, key], |row| {
            decode(row.get::<_, String>(0)?)
        })
        .optional()
}

#[hotpath::measure(label = "rusqlite.external_source.verify_encoded_row")]
pub(super) fn verify_encoded_row<K: rusqlite::ToSql + ?Sized>(
    connection: &rusqlite::Connection,
    sql: &str,
    binding_id: &str,
    key: &K,
    expected: &str,
    collision: &'static str,
) -> rusqlite::Result<()> {
    let stored: String = connection.query_row(sql, params![binding_id, key], |row| row.get(0))?;
    if stored == expected {
        Ok(())
    } else {
        Err(invalid(collision))
    }
}

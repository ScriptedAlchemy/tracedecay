use super::*;

pub(super) fn load_next_pending_projection(
    connection: &rusqlite::Connection,
    state: &SourceStoreStateV1,
) -> rusqlite::Result<Option<SourcePendingProjectionV1>> {
    let binding = state.binding().immutable_identity().map_err(invalid)?;
    let predecessor = frontier_key(
        state
            .projection()
            .map(|projection| projection.source_frontier()),
    );
    let receipt_digest = connection
        .prepare(
            "SELECT source_receipt_digest
             FROM external_source_pending_projections_v1
             WHERE binding_id = ?1 AND predecessor_frontier_digest = ?2",
        )?
        .query_row(params![binding.binding_id.as_str(), predecessor], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    let Some(receipt_digest) = receipt_digest else {
        return Ok(None);
    };
    let receipt = load_commit_receipt_by_digest(connection, &binding, &receipt_digest)?
        .ok_or_else(|| invalid("external source pending receipt is missing"))?;
    let definition = load_definition(
        connection,
        state.definition().source_id.as_str(),
        i64::try_from(receipt.definition_revision())
            .map_err(|_| invalid("external source definition revision exceeds SQLite INTEGER"))?,
    )?;
    let source_binding = load_binding(
        connection,
        binding.binding_id.as_str(),
        i64::try_from(receipt.binding_revision())
            .map_err(|_| invalid("external source binding revision exceeds SQLite INTEGER"))?,
    )?;
    SourcePendingProjectionV1::from_state(state, definition, source_binding, receipt)
        .map(Some)
        .map_err(invalid)
}

pub(super) fn load_next_pending_projection_any(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<Option<SourcePendingProjectionV1>> {
    let binding = connection
        .prepare(
            "SELECT revisions.binding_json
             FROM external_source_pending_projections_v1 AS pending
             JOIN external_source_states_v1 AS states
               ON states.binding_id = pending.binding_id
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
    load_state(connection, &identity)?
        .as_ref()
        .map(|state| load_next_pending_projection(connection, state))
        .transpose()
        .map(Option::flatten)
}

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

pub(super) fn load_projection_receipt(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
    digest: &tracedecay_domain::ManifestDigest,
) -> rusqlite::Result<Option<SourceProjectionCommitV1>> {
    load_projection_receipt_by_digest(connection, binding, digest.as_str())
}

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

fn load_encoded_optional<T: serde::de::DeserializeOwned>(
    connection: &rusqlite::Connection,
    sql: &str,
    binding_id: &str,
    key: &str,
) -> rusqlite::Result<Option<T>> {
    connection
        .prepare(sql)?
        .query_row(params![binding_id, key], |row| {
            decode(row.get::<_, String>(0)?)
        })
        .optional()
}

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

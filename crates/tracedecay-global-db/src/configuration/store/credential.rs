//! Opaque credential reference persistence.

use super::audit::{insert_audit_event_with_receipt_digest, seal_audit_target};
use super::codec::projection_encoding;
use super::mutation::{current_state_from_transaction, derived_identifier, map_store_error};
use super::{
    ConfigurationAuditEvent, ConfigurationAuditEventId, ConfigurationAuditEventKindV1,
    ConfigurationError, ConfigurationIdempotencyKey, ConfigurationMutationAuthority,
    ConfigurationOperationFuture, ConfigurationRevisionId, CredentialKindV1, CredentialReferenceId,
    CredentialReferenceMetadataV1, CredentialWritePort, GlobalDbConfigurationControlStore,
    ManifestDigest, QueryExecutor, UtcMicros, WriteOnlyCredentialMutation, canonical_sha256,
    params,
};

pub(super) async fn credential_reference_from_transaction(
    transaction: &impl QueryExecutor,
    reference_id: &CredentialReferenceId,
) -> Result<Option<CredentialReferenceMetadataV1>, ConfigurationError> {
    let mut rows = transaction
        .query(
            "SELECT kind, reference_digest, created_at, rotation
             FROM configuration_credential_references
             WHERE reference_id = ?1",
            params![reference_id.as_str()],
        )
        .await
        .map_err(|_| ConfigurationError::Unavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| ConfigurationError::Unavailable)?
    else {
        return Ok(None);
    };
    let kind = row
        .get::<String>(0)
        .map_err(|_| ConfigurationError::Unavailable)?;
    let kind = serde_json::from_value::<CredentialKindV1>(serde_json::Value::String(kind))
        .map_err(|_| ConfigurationError::validation_message("stored credential kind is invalid"))?;
    let reference_digest = ManifestDigest::new(
        row.get::<String>(1)
            .map_err(|_| ConfigurationError::Unavailable)?,
    )
    .map_err(ConfigurationError::validation)?;
    let created_at = UtcMicros(
        row.get::<i64>(2)
            .map_err(|_| ConfigurationError::Unavailable)?,
    );
    let rotation = u64::try_from(
        row.get::<i64>(3)
            .map_err(|_| ConfigurationError::Unavailable)?,
    )
    .map_err(|_| ConfigurationError::validation_message("stored credential rotation is invalid"))?;
    let metadata = CredentialReferenceMetadataV1::new(
        reference_id.clone(),
        kind,
        reference_digest,
        created_at,
        rotation,
    )
    .map_err(ConfigurationError::validation)?;
    if rows
        .next()
        .await
        .map_err(|_| ConfigurationError::Unavailable)?
        .is_some()
    {
        return Err(ConfigurationError::validation_message(
            "credential reference resolved to multiple rows",
        ));
    }
    Ok(Some(metadata))
}

impl CredentialWritePort for GlobalDbConfigurationControlStore<'_> {
    fn write_reference(
        &self,
        authority: &ConfigurationMutationAuthority,
        write: &WriteOnlyCredentialMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, CredentialReferenceMetadataV1> {
        let authority = authority.clone();
        let write = write.clone();
        let expected_revision = expected_revision.clone();
        Box::pin(async move {
            authority.validate_integrity()?;
            expected_revision
                .validate()
                .map_err(ConfigurationError::validation)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                let current = current_state_from_transaction(&transaction).await?;
                if current.revision_id != expected_revision {
                    return Err(ConfigurationError::RevisionConflict);
                }
                let prior = match &write.expected_reference_id {
                    Some(reference_id) => {
                        let prior =
                            credential_reference_from_transaction(&transaction, reference_id)
                                .await?
                                .ok_or(ConfigurationError::PlanStale)?;
                        if prior.kind != write.kind {
                            return Err(ConfigurationError::IdempotencyConflict);
                        }
                        prior
                    }
                    None => CredentialReferenceMetadataV1::new(
                        CredentialReferenceId::new("credential.reference.none")
                            .map_err(ConfigurationError::validation)?,
                        write.kind.clone(),
                        canonical_sha256(&(
                            "tracedecay.configuration.empty-credential-reference.v1",
                        ))
                        .map_err(ConfigurationError::validation)?,
                        UtcMicros(0),
                        0,
                    )
                    .map_err(ConfigurationError::validation)?,
                };
                let rotation = if write.expected_reference_id.is_some() {
                    prior.rotation.checked_add(1).ok_or_else(|| {
                        ConfigurationError::validation_message("credential rotation overflow")
                    })?
                } else {
                    0
                };
                let reference_digest = canonical_sha256(&(
                    "tracedecay.configuration.credential-reference.v1",
                    &authority.receipt.receipt_id,
                    &write.kind,
                    write.write_handle.as_str(),
                    &write.expected_reference_id,
                    rotation,
                ))
                .map_err(ConfigurationError::validation)?;
                let operation_digest = canonical_sha256(&(
                    "tracedecay.configuration.credential-write.v1",
                    &authority.receipt.receipt_id,
                    &expected_revision,
                    &write.kind,
                    write.write_handle.as_str(),
                    &write.expected_reference_id,
                    rotation,
                ))
                .map_err(ConfigurationError::validation)?;
                let idempotency_key: ConfigurationIdempotencyKey = derived_identifier(
                    "configuration.idempotency.credential-write.v1",
                    &canonical_sha256(&(
                        "tracedecay.configuration.credential-write-idempotency.v1",
                        &authority.receipt.receipt_id,
                        &operation_digest,
                    ))
                    .map_err(ConfigurationError::validation)?,
                    "credential write idempotency key",
                )?;
                let reference_id: CredentialReferenceId = derived_identifier(
                    "credential.reference.v1",
                    &canonical_sha256(&(
                        "tracedecay.configuration.credential-reference-id.v1",
                        &authority.receipt.receipt_id,
                        &reference_digest,
                    ))
                    .map_err(ConfigurationError::validation)?,
                    "credential reference id",
                )?;
                let metadata = CredentialReferenceMetadataV1::new(
                    reference_id.clone(),
                    write.kind.clone(),
                    reference_digest,
                    authority.receipt.issued_at,
                    rotation,
                )
                .map_err(ConfigurationError::validation)?;
                match credential_reference_from_transaction(&transaction, &reference_id).await? {
                    Some(existing) if existing == metadata => Ok(existing),
                    Some(_) => Err(ConfigurationError::IdempotencyConflict),
                    None => {
                        transaction
                            .execute(
                                "INSERT INTO configuration_credential_references (
                                    reference_id, kind, reference_digest, created_at, rotation
                                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                                params![
                                    metadata.reference_id.as_str(),
                                    projection_encoding(&metadata.kind).map_err(map_store_error)?,
                                    metadata.reference_digest.as_str(),
                                    metadata.created_at.0,
                                    i64::try_from(metadata.rotation).map_err(|_| {
                                        ConfigurationError::validation_message(
                                            "credential rotation exceeds SQLite range",
                                        )
                                    })?,
                                ],
                            )
                            .await
                            .map_err(|_| ConfigurationError::Unavailable)?;
                        let event_id: ConfigurationAuditEventId = derived_identifier(
                            "configuration.audit.v1",
                            &canonical_sha256(&(
                                "tracedecay.configuration.credential-write-audit.v1",
                                &authority.receipt.actor_id,
                                &idempotency_key,
                                &operation_digest,
                            ))
                            .map_err(ConfigurationError::validation)?,
                            "configuration audit event id",
                        )?;
                        let (sealed_target_reference, target_commitment) = seal_audit_target(
                            &transaction,
                            &event_id,
                            &metadata,
                            authority.receipt.issued_at,
                        )
                        .await
                        .map_err(map_store_error)?;
                        let event = ConfigurationAuditEvent {
                            event_id,
                            event_kind: ConfigurationAuditEventKindV1::Applied,
                            actor_id: authority.receipt.actor_id.clone(),
                            idempotency_key: Some(idempotency_key),
                            base_revision_id: expected_revision.clone(),
                            result_revision_id: None,
                            operation_digest,
                            target_commitment,
                            receipt_id: None,
                            safe_reason_code: None,
                            occurred_at: authority.receipt.issued_at,
                        };
                        insert_audit_event_with_receipt_digest(
                            &transaction,
                            &event,
                            None,
                            Some(&sealed_target_reference),
                        )
                        .await
                        .map_err(map_store_error)?;
                        Ok(metadata)
                    }
                }
            }
            .await;
            match outcome {
                Ok(metadata) => transaction
                    .commit()
                    .await
                    .map(|()| metadata)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }
}

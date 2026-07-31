use super::*;

pub(super) fn encode_audit_payload(
    event: &ConfigurationAuditEvent,
) -> ConfigurationStoreResult<String> {
    serde_json::to_string(&StoredConfigurationAuditPayloadV1 {
        schema_version: CONFIGURATION_AUDIT_PAYLOAD_SCHEMA_VERSION,
        event: event.clone(),
    })
    .map_err(|error| invalid_store_data(format!("encode configuration audit payload: {error}")))
}

const CONFIGURATION_AUDIT_REDACTION_KEY_BYTES: usize = 32;

async fn read_audit_redaction_key(
    transaction: &impl QueryExecutor,
) -> ConfigurationStoreResult<Option<Zeroizing<Vec<u8>>>> {
    let mut rows = transaction
        .query(
            "SELECT key_material FROM configuration_audit_redaction_keys WHERE singleton = 1",
            (),
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let material = Zeroizing::new(row.get::<Vec<u8>>(0).map_err(|error| {
        invalid_store_data(format!("read configuration audit redaction key: {error}"))
    })?);
    if material.len() != CONFIGURATION_AUDIT_REDACTION_KEY_BYTES
        || rows.next().await.map_err(unavailable_store)?.is_some()
    {
        return Err(invalid_store_data(
            "configuration audit redaction key is not canonical",
        ));
    }
    Ok(Some(material))
}

async fn ensure_audit_redaction_key(
    transaction: &impl Executor,
    created_at: UtcMicros,
) -> ConfigurationStoreResult<Zeroizing<Vec<u8>>> {
    if let Some(material) = read_audit_redaction_key(transaction).await? {
        return Ok(material);
    }
    let mut material = Zeroizing::new(vec![0_u8; CONFIGURATION_AUDIT_REDACTION_KEY_BYTES]);
    getrandom::getrandom(material.as_mut_slice())
        .map_err(|_| ConfigurationStoreError::Unavailable)?;
    transaction
        .execute(
            "INSERT INTO configuration_audit_redaction_keys (singleton, key_material, created_at)
             VALUES (1, ?1, ?2)",
            params![material.as_slice(), created_at.0],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(material)
}

pub(super) fn audit_target_commitment(
    key: &[u8],
    event_id: &ConfigurationAuditEventId,
    sealed_target_reference: &[u8],
) -> ConfigurationStoreResult<ManifestDigest> {
    let authenticated = serde_json::to_vec(&(
        "tracedecay.configuration.audit-target-commitment.v1",
        event_id,
        sealed_target_reference,
    ))
    .map_err(|error| invalid_store_data(format!("encode audit target commitment: {error}")))?;
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key)
        .map_err(|_| invalid_store_data("configuration audit redaction key is invalid"))?;
    mac.update(&authenticated);
    ManifestDigest::new(format!(
        "sha256:{}",
        hex::encode(mac.finalize().into_bytes())
    ))
    .map_err(ConfigurationStoreError::from)
}

pub(super) async fn seal_audit_target<T: Serialize>(
    transaction: &impl Executor,
    event_id: &ConfigurationAuditEventId,
    target: &T,
    created_at: UtcMicros,
) -> ConfigurationStoreResult<(Vec<u8>, ManifestDigest)> {
    let sealed = serde_json::to_vec(&SealedAuditTargetReferenceV1 {
        schema_version: CONFIGURATION_SEALED_AUDIT_TARGET_SCHEMA_VERSION,
        target,
    })
    .map_err(|error| invalid_store_data(format!("seal configuration audit target: {error}")))?;
    let key = ensure_audit_redaction_key(transaction, created_at).await?;
    let commitment = audit_target_commitment(&key, event_id, &sealed)?;
    Ok((sealed, commitment))
}

pub(super) async fn validate_sealed_audit_target(
    transaction: &impl QueryExecutor,
    event: &ConfigurationAuditEvent,
    sealed_target_reference: Option<&[u8]>,
) -> ConfigurationStoreResult<()> {
    let Some(sealed_target_reference) = sealed_target_reference else {
        return Ok(());
    };
    let key = read_audit_redaction_key(transaction)
        .await?
        .ok_or_else(|| invalid_store_data("configuration audit redaction key is missing"))?;
    let expected = audit_target_commitment(&key, &event.event_id, sealed_target_reference)?;
    if event.target_commitment != expected {
        return Err(invalid_store_data(
            "configuration audit target commitment does not bind its sealed reference",
        ));
    }
    Ok(())
}

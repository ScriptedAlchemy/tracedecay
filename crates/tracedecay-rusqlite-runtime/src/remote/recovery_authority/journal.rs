use super::*;

pub(super) enum BeginOperationV1<T> {
    Completed(RemoteRecoveryCommittedV1<T>),
    Execute { pre_state_digest: ManifestDigest },
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin_operation<T>(
    handle: &ExactSqlHandle,
    kind: &str,
    operation_id: &str,
    input_digest: &ManifestDigest,
    context_json: &str,
    authority_key: &str,
    expected: &RecoveryAuthorityExpectationV1,
    promotion: bool,
    replacement: Option<&RemoteWriterFenceV1>,
    started_at: UtcMicros,
) -> Result<BeginOperationV1<T>, RemoteRecoveryOperationErrorV1>
where
    T: DeserializeOwned,
{
    let transaction = handle
        .begin_immediate()
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    let existing = load_operation::<T>(&transaction, operation_id)?;
    let operation_exists = existing.is_some();
    let mut retained_pre_state_digest = None;
    if let Some(existing) = existing {
        if existing.kind != kind
            || existing.request_digest != *input_digest
            || existing.context_json != context_json
        {
            return Err(RemoteRecoveryOperationErrorV1::Conflict);
        }
        if let Some(mut committed) = existing.committed {
            let (current, _) = load_authority_in(&transaction, authority_key)
                .map_err(map_store_error)?
                .ok_or(RemoteRecoveryOperationErrorV1::StaleAuthority)?;
            committed.authority = CurrentRemoteAuthorityStateV1::Available(current);
            transaction
                .commit()
                .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
            return Ok(BeginOperationV1::Completed(committed));
        }
        if matches!(
            existing.state.as_str(),
            "cancelled" | "timed_out" | "rolled_back"
        ) {
            return Err(match existing.state.as_str() {
                "cancelled" => RemoteRecoveryOperationErrorV1::Cancelled,
                "timed_out" => RemoteRecoveryOperationErrorV1::TimedOut,
                _ => RemoteRecoveryOperationErrorV1::RecoveryRequired,
            });
        }
        retained_pre_state_digest = Some(existing.pre_state_digest);
    }
    let (current, frontier) = load_authority_in(&transaction, authority_key)
        .map_err(map_store_error)?
        .ok_or(RemoteRecoveryOperationErrorV1::StaleAuthority)?;
    let expected_matches = expected.matches_writer(&current.fence);
    let replacement_matches = replacement.is_some_and(|replacement| current.fence == *replacement);
    if !expected_matches && !(promotion && replacement_matches) {
        return Err(RemoteRecoveryOperationErrorV1::StaleAuthority);
    }
    let pre_state_digest = retained_pre_state_digest.unwrap_or(
        canonical_sha256(&(&current, frontier))
            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
    );
    if !operation_exists {
        transaction
            .execute(
                ExactSqlStatement::new(
                    "INSERT INTO remote_recovery_operations (
                        operation_id, operation_kind, request_digest,
                        expected_authority_key, pre_state_digest, state,
                        context_json, output_json, receipt_json, started_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 'executing', ?6, NULL, NULL, ?7, ?7)"
                        .to_owned(),
                    vec![
                        text(operation_id),
                        text(kind),
                        text(input_digest.as_str()),
                        text(authority_key),
                        text(pre_state_digest.as_str()),
                        text(context_json),
                        ExactSqlValue::Integer(started_at.0),
                    ],
                )
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    }
    transaction
        .commit()
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    Ok(BeginOperationV1::Execute { pre_state_digest })
}

pub(super) fn publish_promoted_authorities(
    handle: &ExactSqlHandle,
    authority_key: &str,
    expected: &RecoveryAuthorityExpectationV1,
    replacement: &RemoteWriterFenceV1,
    frontier_sequence: u64,
    observed_at: UtcMicros,
) -> Result<(), RemoteRecoveryOperationErrorV1> {
    let transaction = handle
        .begin_immediate()
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    let (current, stored_frontier) = load_authority_in(&transaction, authority_key)
        .map_err(map_store_error)?
        .ok_or(RemoteRecoveryOperationErrorV1::StaleAuthority)?;
    if stored_frontier > frontier_sequence {
        return Err(RemoteRecoveryOperationErrorV1::Conflict);
    }
    if current.fence != *replacement {
        if !expected.matches_writer(&current.fence) {
            return Err(RemoteRecoveryOperationErrorV1::StaleAuthority);
        }
        let replacement_authority = CurrentRemoteAuthorityV1 {
            fence: replacement.clone(),
            credential_revision: current.credential_revision,
            observed_at,
        };
        let result = transaction
            .execute(
                ExactSqlStatement::new(
                    "UPDATE remote_recovery_authorities
                     SET authority_json = ?1, frontier_sequence = ?2, updated_at = ?3
                     WHERE authority_key = ?4 AND authority_json = ?5
                       AND frontier_sequence = ?6"
                        .to_owned(),
                    vec![
                        text(
                            &serde_json::to_string(&replacement_authority)
                                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
                        ),
                        integer(frontier_sequence)?,
                        ExactSqlValue::Integer(observed_at.0),
                        text(authority_key),
                        text(
                            &serde_json::to_string(&current)
                                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
                        ),
                        integer(stored_frontier)?,
                    ],
                )
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
        if result.changed_rows != 1 {
            return Err(RemoteRecoveryOperationErrorV1::Conflict);
        }
    }
    promote_primary_writer_in(&transaction, expected, replacement, observed_at)?;
    transaction
        .commit()
        .map(|_receipt| ())
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)
}

fn promote_primary_writer_in(
    transaction: &ExactSqlTransaction,
    expected: &RecoveryAuthorityExpectationV1,
    replacement: &RemoteWriterFenceV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteRecoveryOperationErrorV1> {
    let rows = transaction
        .query(
            ExactSqlStatement::new(
                "SELECT authority_state_json, writer_json
                 FROM remote_authorities WHERE brain_id = ?1"
                    .to_owned(),
                vec![text(&expected.brain_id)],
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
        )
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    let mut rows = rows.rows.into_iter();
    let Some(row) = rows.next() else {
        return Err(RemoteRecoveryOperationErrorV1::StaleAuthority);
    };
    if rows.next().is_some() {
        return Err(RemoteRecoveryOperationErrorV1::Corruption);
    }
    let encoded_state = exact_text(&row, 0)?;
    let encoded_writer = exact_text(&row, 1)?;
    let state: CurrentRemoteAuthorityStateV1 = serde_json::from_str(encoded_state)
        .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
    let mut writer: RemoteWriterAuthorityV1 = serde_json::from_str(encoded_writer)
        .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
    let CurrentRemoteAuthorityStateV1::Available(current) = state else {
        return Err(RemoteRecoveryOperationErrorV1::StaleAuthority);
    };
    if writer.authority.fence == *replacement && current.fence == *replacement {
        return Ok(());
    }
    if !expected.matches_writer(&writer.authority.fence)
        || !expected.matches_writer(&current.fence)
        || writer.authority != current
    {
        return Err(RemoteRecoveryOperationErrorV1::StaleAuthority);
    }
    writer.authority.fence = replacement.clone();
    writer.authority.observed_at = observed_at;
    let replacement_state = CurrentRemoteAuthorityStateV1::Available(writer.authority.clone());
    let changed = transaction
        .execute(
            ExactSqlStatement::new(
                "UPDATE remote_authorities
                 SET authority_state_json = ?1, writer_json = ?2, updated_at = ?3
                 WHERE brain_id = ?4
                   AND authority_state_json = ?5 AND writer_json = ?6"
                    .to_owned(),
                vec![
                    text(
                        &serde_json::to_string(&replacement_state)
                            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
                    ),
                    text(
                        &serde_json::to_string(&writer)
                            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
                    ),
                    ExactSqlValue::Integer(observed_at.0),
                    text(&expected.brain_id),
                    text(encoded_state),
                    text(encoded_writer),
                ],
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
        )
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    if changed.changed_rows == 1 {
        Ok(())
    } else {
        Err(RemoteRecoveryOperationErrorV1::Conflict)
    }
}

struct LoadedOperationV1<T> {
    kind: String,
    request_digest: ManifestDigest,
    state: String,
    pre_state_digest: ManifestDigest,
    context_json: String,
    committed: Option<RemoteRecoveryCommittedV1<T>>,
}

fn load_operation<T>(
    transaction: &ExactSqlTransaction,
    operation_id: &str,
) -> Result<Option<LoadedOperationV1<T>>, RemoteRecoveryOperationErrorV1>
where
    T: DeserializeOwned,
{
    let rows = transaction
        .query(
            ExactSqlStatement::new(
                "SELECT operation_kind, request_digest, state, pre_state_digest,
                        context_json, output_json, receipt_json
                 FROM remote_recovery_operations WHERE operation_id = ?1"
                    .to_owned(),
                vec![text(operation_id)],
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
        )
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    let mut rows = rows.rows.into_iter();
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    if rows.next().is_some() {
        return Err(RemoteRecoveryOperationErrorV1::Corruption);
    }
    let kind = exact_text(&row, 0)?.to_owned();
    let request_digest = ManifestDigest::new(exact_text(&row, 1)?.to_owned())
        .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
    let state = exact_text(&row, 2)?.to_owned();
    let pre_state_digest = ManifestDigest::new(exact_text(&row, 3)?.to_owned())
        .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
    let context_json = exact_text(&row, 4)?.to_owned();
    let committed = match (row.values.get(5), row.values.get(6), state.as_str()) {
        (Some(ExactSqlValue::Text(output)), Some(ExactSqlValue::Text(receipt)), "completed") => {
            let output: T = serde_json::from_str(output)
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
            let receipt: RemoteRecoveryOperationReceiptV1 = serde_json::from_str(receipt)
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
            receipt
                .validate()
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
            Some(RemoteRecoveryCommittedV1 {
                authority: CurrentRemoteAuthorityStateV1::Available(
                    current_authority_from_receipt(&receipt)?,
                ),
                receipt,
                output,
            })
        }
        (Some(ExactSqlValue::Null), Some(ExactSqlValue::Null), _) => None,
        _ => return Err(RemoteRecoveryOperationErrorV1::Corruption),
    };
    Ok(Some(LoadedOperationV1 {
        kind,
        request_digest,
        state,
        pre_state_digest,
        context_json,
        committed,
    }))
}

pub(super) fn finish_operation<T: Serialize>(
    handle: &ExactSqlHandle,
    operation_id: &str,
    input_digest: &ManifestDigest,
    output: &T,
    receipt: &RemoteRecoveryOperationReceiptV1,
) -> Result<(), RemoteRecoveryOperationErrorV1> {
    let result = handle
        .execute(
            ExactSqlStatement::new(
                "UPDATE remote_recovery_operations
                 SET state = 'completed', output_json = ?1, receipt_json = ?2, updated_at = ?3
                 WHERE operation_id = ?4 AND request_digest = ?5
                   AND state IN ('executing', 'forward_recovery_required')"
                    .to_owned(),
                vec![
                    text(
                        &serde_json::to_string(output)
                            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
                    ),
                    text(
                        &serde_json::to_string(receipt)
                            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
                    ),
                    ExactSqlValue::Integer(receipt.committed_at.0),
                    text(operation_id),
                    text(input_digest.as_str()),
                ],
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
        )
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    if result.changed_rows == 1 {
        Ok(())
    } else {
        Err(RemoteRecoveryOperationErrorV1::Conflict)
    }
}

pub(super) fn record_interruption(
    handle: &ExactSqlHandle,
    operation_id: &str,
    input_digest: &ManifestDigest,
    interruption: RemoteRecoveryInterruptionV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteRecoveryOperationErrorV1> {
    let state = match interruption {
        RemoteRecoveryInterruptionV1::Cancelled => "cancelled",
        RemoteRecoveryInterruptionV1::DeadlineExceeded => "timed_out",
    };
    update_operation_state(handle, operation_id, input_digest, state, observed_at)
}

pub(super) fn record_physical_failure(
    handle: &ExactSqlHandle,
    operation_id: &str,
    input_digest: &ManifestDigest,
    error: RemoteRecoveryPhysicalEffectErrorV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteRecoveryOperationErrorV1> {
    let state = match error {
        RemoteRecoveryPhysicalEffectErrorV1::RolledBack => "rolled_back",
        RemoteRecoveryPhysicalEffectErrorV1::Cancelled => "cancelled",
        RemoteRecoveryPhysicalEffectErrorV1::TimedOut => "timed_out",
        RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired
        | RemoteRecoveryPhysicalEffectErrorV1::Unavailable
        | RemoteRecoveryPhysicalEffectErrorV1::Corruption => "forward_recovery_required",
    };
    update_operation_state(handle, operation_id, input_digest, state, observed_at)
}

fn update_operation_state(
    handle: &ExactSqlHandle,
    operation_id: &str,
    input_digest: &ManifestDigest,
    state: &str,
    observed_at: UtcMicros,
) -> Result<(), RemoteRecoveryOperationErrorV1> {
    let result = handle
        .execute(
            ExactSqlStatement::new(
                "UPDATE remote_recovery_operations SET state = ?1, updated_at = ?2
                 WHERE operation_id = ?3 AND request_digest = ?4
                   AND state IN ('executing', 'forward_recovery_required')"
                    .to_owned(),
                vec![
                    text(state),
                    ExactSqlValue::Integer(observed_at.0),
                    text(operation_id),
                    text(input_digest.as_str()),
                ],
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
        )
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    if result.changed_rows == 1 {
        Ok(())
    } else {
        Err(RemoteRecoveryOperationErrorV1::Conflict)
    }
}

pub(super) fn load_authority_in(
    transaction: &ExactSqlTransaction,
    authority_key: &str,
) -> Result<Option<(CurrentRemoteAuthorityV1, u64)>, RemoteSqliteStorageErrorV1> {
    let rows = transaction.query(ExactSqlStatement::new(
        "SELECT authority_json, frontier_sequence
         FROM remote_recovery_authorities WHERE authority_key = ?1"
            .to_owned(),
        vec![text(authority_key)],
    )?)?;
    let mut rows = rows.rows.into_iter();
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    if rows.next().is_some() {
        return Err(RemoteSqliteStorageErrorV1::Corruption);
    }
    let authority = serde_json::from_str(exact_text_store(&row, 0)?)
        .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    let frontier = exact_u64_store(&row, 1)?;
    Ok(Some((authority, frontier)))
}

pub(super) fn map_store_error(
    error: RemoteSqliteStorageErrorV1,
) -> RemoteRecoveryOperationErrorV1 {
    match error {
        RemoteSqliteStorageErrorV1::Corruption => RemoteRecoveryOperationErrorV1::Corruption,
        _ => RemoteRecoveryOperationErrorV1::Unavailable,
    }
}

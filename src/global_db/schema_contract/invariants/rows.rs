use libsql::{Connection, params};
use serde::{Serialize, de::DeserializeOwned};
use tracedecay_domain::{
    DurableObservationV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, SanitizationReceiptV1, SanitizerDispositionV1,
};
use tracedecay_store::observation::ObservationCoverageV1;

use crate::global_db::{global_db_operation_error, global_db_operation_message};

use super::OPERATION;
use super::triggers::INVARIANTS;

pub(super) async fn query_has_rows(conn: &Connection, query: &str) -> crate::errors::Result<bool> {
    let mut rows = conn
        .query(query, ())
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(super) fn authority_violation(message: impl Into<String>) -> crate::errors::TraceDecayError {
    global_db_operation_message(OPERATION, message)
}

pub(super) fn decode_authority_json<T: DeserializeOwned>(
    json: &str,
    authority: &str,
) -> crate::errors::Result<T> {
    serde_json::from_str(json)
        .map_err(|error| authority_violation(format!("invalid {authority}: {error}")))
}

pub(super) fn encode_authority_json<T: Serialize>(
    value: &T,
    authority: &str,
) -> crate::errors::Result<String> {
    serde_json::to_string(value)
        .map_err(|error| authority_violation(format!("cannot encode {authority}: {error}")))
}

pub(super) async fn validate_receipt_authority_rows(
    conn: &Connection,
    after_rowid: i64,
) -> crate::errors::Result<(i64, i64)> {
    let mut rows = conn
        .query(
            "SELECT rowid, receipt_id, sanitizer_version, payload_digest, receipt_json
             FROM sanitization_receipts WHERE rowid > ?1 ORDER BY rowid",
            params![after_rowid],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut high_water = after_rowid;
    let mut audited = 0;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        high_water = row
            .get::<i64>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let receipt_id = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let sanitizer_version = row
            .get::<String>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let payload_digest = row
            .get::<String>(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let receipt_json = row
            .get::<String>(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let receipt: SanitizationReceiptV1 =
            decode_authority_json(&receipt_json, "sanitization receipt authority JSON")?;
        let receipt_ref = receipt.receipt();
        let expected_payload_digest = receipt
            .payload()
            .map_or("", |payload| payload.digest().as_str());
        if receipt_ref.receipt_id().as_str() != receipt_id
            || receipt_ref.sanitizer_version().as_str() != sanitizer_version
            || expected_payload_digest != payload_digest
        {
            return Err(authority_violation(
                "sanitization receipt authority columns disagree with receipt JSON",
            ));
        }
        audited += 1;
    }
    Ok((high_water, audited))
}

pub(super) async fn validate_observation_authority_rows(
    conn: &Connection,
    after_sequence: i64,
) -> crate::errors::Result<(i64, i64)> {
    let mut rows = conn
        .query(
            "SELECT observation.sequence, observation.observation_id,
                    observation.payload_digest, observation.receipt_id,
                    observation.observation_json, observation.committed_cursor_json,
                    receipt.receipt_json
             FROM observations AS observation
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = observation.receipt_id
             WHERE observation.sequence > ?1 ORDER BY observation.sequence",
            params![after_sequence],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut high_water = after_sequence;
    let mut audited = 0;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let sequence = row
            .get::<i64>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        high_water = sequence;
        let observation_id = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let payload_digest = row
            .get::<String>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let receipt_id = row
            .get::<String>(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let observation_json = row
            .get::<String>(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let cursor_json = row
            .get::<String>(5)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(receipt_json) = row
            .get::<Option<String>>(6)
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        else {
            return Err(authority_violation(
                "committed observation references a missing receipt",
            ));
        };

        let observation: DurableObservationV1 =
            decode_authority_json(&observation_json, "committed observation authority JSON")?;
        let cursor: ObservationSourceCursorV1 =
            decode_authority_json(&cursor_json, "committed source cursor authority JSON")?;
        let stored_receipt: SanitizationReceiptV1 =
            decode_authority_json(&receipt_json, "sanitization receipt authority JSON")?;
        if sequence <= 0
            || observation.observation_id().as_str() != observation_id
            || observation.payload_reference().digest().as_str() != payload_digest
            || observation.receipt().receipt().receipt_id().as_str() != receipt_id
            || observation.receipt() != &stored_receipt
        {
            return Err(authority_violation(
                "committed observation authority columns disagree with observation JSON",
            ));
        }
        if cursor.source() != observation.source()
            || cursor.scope() != observation.scope()
            || cursor.generation() != observation.identity().generation()
            || cursor.position() != observation.identity().position().end()
        {
            return Err(authority_violation(
                "committed source cursor disagrees with observation source evidence",
            ));
        }
        audited += 1;
    }
    Ok((high_water, audited))
}

pub(super) async fn validate_source_cursor_authority_rows(
    conn: &Connection,
) -> crate::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT source_json, scope_json, cursor_json FROM source_cursors",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let source_json = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let scope_json = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let cursor_json = row
            .get::<String>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let source: ObservationSourceIdentityV1 =
            decode_authority_json(&source_json, "source cursor identity JSON")?;
        let scope: ObservationScopeV1 =
            decode_authority_json(&scope_json, "source cursor scope JSON")?;
        let cursor: ObservationSourceCursorV1 =
            decode_authority_json(&cursor_json, "source cursor authority JSON")?;
        if cursor.source() != &source
            || cursor.scope() != &scope
            || source_json != encode_authority_json(&source, "source cursor identity JSON")?
            || scope_json != encode_authority_json(&scope, "source cursor scope JSON")?
            || cursor_json != encode_authority_json(&cursor, "source cursor authority JSON")?
        {
            return Err(authority_violation(
                "source cursor authority keys disagree with cursor JSON",
            ));
        }
    }
    drop(rows);

    let mut rows = conn
        .query(
            "SELECT advance.source_json, advance.scope_json, advance.coverage_json,
                    advance.reason, advance.receipt_id, receipt.receipt_json
             FROM source_cursor_advances AS advance
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = advance.receipt_id",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let source_json = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let scope_json = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let coverage_json = row
            .get::<String>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let reason = row
            .get::<String>(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let receipt_id = row
            .get::<Option<String>>(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let receipt_json = row
            .get::<Option<String>>(5)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let source: ObservationSourceIdentityV1 =
            decode_authority_json(&source_json, "source cursor advance identity JSON")?;
        let scope: ObservationScopeV1 =
            decode_authority_json(&scope_json, "source cursor advance scope JSON")?;
        let coverage: ObservationCoverageV1 =
            decode_authority_json(&coverage_json, "source cursor advance coverage JSON")?;
        let receipt_matches = match (reason.as_str(), receipt_id, receipt_json) {
            (
                "blank_frame" | "out_of_scope" | "malformed_frame" | "oversized_frame"
                | "unknown_version" | "unsupported_fact",
                None,
                None,
            ) => true,
            (
                "sanitizer_rejected" | "sanitizer_quarantined" | "duplicate_observation",
                Some(receipt_id),
                Some(receipt_json),
            ) => {
                let receipt: SanitizationReceiptV1 = decode_authority_json(
                    &receipt_json,
                    "source cursor advance sanitization receipt JSON",
                )?;
                let disposition_matches = match reason.as_str() {
                    "sanitizer_rejected" => {
                        receipt.disposition() == SanitizerDispositionV1::Rejected
                    }
                    "sanitizer_quarantined" => {
                        receipt.disposition() == SanitizerDispositionV1::Quarantined
                    }
                    "duplicate_observation" => matches!(
                        receipt.disposition(),
                        SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted
                    ),
                    _ => false,
                };
                receipt.receipt().receipt_id().as_str() == receipt_id
                    && disposition_matches
                    && (reason == "duplicate_observation") == receipt.payload().is_some()
            }
            _ => false,
        };
        if source_json != encode_authority_json(&source, "source cursor advance identity JSON")?
            || scope_json != encode_authority_json(&scope, "source cursor advance scope JSON")?
            || coverage_json
                != encode_authority_json(&coverage, "source cursor advance coverage JSON")?
            || !matches!(
                reason.as_str(),
                "blank_frame"
                    | "out_of_scope"
                    | "malformed_frame"
                    | "oversized_frame"
                    | "unknown_version"
                    | "unsupported_fact"
                    | "duplicate_observation"
                    | "sanitizer_rejected"
                    | "sanitizer_quarantined"
            )
            || !receipt_matches
        {
            return Err(authority_violation(
                "source cursor advance contains invalid authority evidence",
            ));
        }
    }
    Ok(())
}

pub(super) async fn validate_mutable_invariant_rows(
    conn: &Connection,
) -> crate::errors::Result<()> {
    for invariant in [
        &INVARIANTS[4],
        &INVARIANTS[5],
        &INVARIANTS[9],
        &INVARIANTS[12],
    ] {
        if let Some(query) = invariant.audit_query
            && query_has_rows(conn, query).await?
        {
            return Err(global_db_operation_message(OPERATION, invariant.violation));
        }
    }
    Ok(())
}

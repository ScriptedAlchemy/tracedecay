//! Durable single-use handoff opens on the canonical registered SQL channel.
//!
//! This authority stores only secret-free token digests and bindings. It uses
//! the registered exact SQL handle; it never opens a database or creates a
//! parallel authority.

use std::time::Duration;

use tracedecay_application::{
    HandoffOpenAuthorityError, HandoffOpenAuthorityPort, HandoffOpenConsumeOutcomeV1,
    HandoffOpenConsumptionV1, HandoffOpenContextV1, HandoffOpenGrantV1, RequestId,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};

use crate::exact_sql::{
    ExactSqlError, ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction,
    ExactSqlValue,
};
const HANDOFF_OPEN_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS handoff_open_grants_v1 (
    token_digest TEXT NOT NULL PRIMARY KEY,
    grant_payload TEXT NOT NULL,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > issued_at),
    consumed_request_id TEXT,
    consumed_input_digest TEXT,
    consumption_payload TEXT,
    CHECK (
        (consumed_request_id IS NULL
         AND consumed_input_digest IS NULL
         AND consumption_payload IS NULL)
        OR
        (consumed_request_id IS NOT NULL
         AND consumed_input_digest IS NOT NULL
         AND consumption_payload IS NOT NULL)
    )
) STRICT;
";

#[derive(Clone)]
pub struct HandoffOpenSqliteAuthority {
    handle: ExactSqlHandle,
}

impl HandoffOpenSqliteAuthority {
    pub fn from_registered(
        handle: ExactSqlHandle,
    ) -> Result<Self, HandoffOpenSqliteAuthorityBuildError> {
        let authority = Self { handle };
        authority
            .handle
            .execute_batch(HANDOFF_OPEN_SCHEMA_V1.to_owned())
            .map_err(|_| HandoffOpenSqliteAuthorityBuildError::Unavailable)?;
        Ok(authority)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffOpenSqliteAuthorityBuildError {
    Unavailable,
}

fn unavailable(_: ExactSqlError) -> HandoffOpenAuthorityError {
    HandoffOpenAuthorityError::Unavailable
}

fn codec_unavailable() -> HandoffOpenAuthorityError {
    HandoffOpenAuthorityError::Unavailable
}

fn statement(sql: &str, params: Vec<ExactSqlValue>) -> Result<ExactSqlStatement, ExactSqlError> {
    ExactSqlStatement::new(sql.to_owned(), params)
}

fn query_handle(
    handle: &ExactSqlHandle,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, ExactSqlError> {
    handle.query(statement(sql, params)?, Duration::from_secs(5))
}

fn query_tx(
    transaction: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, ExactSqlError> {
    transaction.query(statement(sql, params)?)
}

fn execute_tx(
    transaction: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<(), ExactSqlError> {
    transaction.execute(statement(sql, params)?).map(|_| ())
}

fn text(values: &[ExactSqlValue], index: usize) -> Option<&str> {
    match values.get(index)? {
        ExactSqlValue::Text(value) => Some(value),
        _ => None,
    }
}

fn optional_text(values: &[ExactSqlValue], index: usize) -> Result<Option<&str>, ()> {
    match values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(Some(value)),
        Some(ExactSqlValue::Null) => Ok(None),
        _ => Err(()),
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, HandoffOpenAuthorityError> {
    serde_json::to_string(value).map_err(|_| codec_unavailable())
}

fn decode<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, HandoffOpenAuthorityError> {
    serde_json::from_str(payload).map_err(|_| codec_unavailable())
}

impl HandoffOpenAuthorityPort for HandoffOpenSqliteAuthority {
    fn issue(&self, grant: &HandoffOpenGrantV1) -> Result<(), HandoffOpenAuthorityError> {
        let payload = encode(grant)?;
        let transaction = self.handle.begin_immediate().map_err(unavailable)?;
        let existing = query_tx(
            &transaction,
            "SELECT 1 FROM handoff_open_grants_v1 WHERE token_digest = ?1",
            vec![ExactSqlValue::Text(
                grant.token_digest().as_str().to_owned(),
            )],
        )
        .map_err(unavailable)?;
        if !existing.rows.is_empty() {
            let _ = transaction.rollback();
            return Err(HandoffOpenAuthorityError::Conflict);
        }
        execute_tx(
            &transaction,
            "INSERT INTO handoff_open_grants_v1 (
                 token_digest, grant_payload, issued_at, expires_at,
                 consumed_request_id, consumed_input_digest, consumption_payload
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL)",
            vec![
                ExactSqlValue::Text(grant.token_digest().as_str().to_owned()),
                ExactSqlValue::Text(payload),
                ExactSqlValue::Integer(grant.issued_at().0),
                ExactSqlValue::Integer(grant.expires_at().0),
            ],
        )
        .map_err(unavailable)?;
        transaction.commit().map(|_| ()).map_err(unavailable)
    }

    fn resolve(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenContextV1,
        observed_at: UtcMicros,
    ) -> Result<Option<HandoffOpenGrantV1>, HandoffOpenAuthorityError> {
        let rows = query_handle(
            &self.handle,
            "SELECT grant_payload FROM handoff_open_grants_v1 WHERE token_digest = ?1",
            vec![ExactSqlValue::Text(token_digest.as_str().to_owned())],
        )
        .map_err(unavailable)?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let payload = text(&row.values, 0).ok_or_else(codec_unavailable)?;
        let grant: HandoffOpenGrantV1 = decode(payload)?;
        if grant.context() != expected || observed_at >= *grant.expires_at() {
            return Ok(None);
        }
        Ok(Some(grant))
    }

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenContextV1,
        request_id: &RequestId,
        input_digest: &ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<HandoffOpenConsumeOutcomeV1, HandoffOpenAuthorityError> {
        let transaction = self.handle.begin_immediate().map_err(unavailable)?;
        let rows = query_tx(
            &transaction,
            "SELECT grant_payload, consumed_request_id, consumed_input_digest,
                    consumption_payload
             FROM handoff_open_grants_v1
             WHERE token_digest = ?1",
            vec![ExactSqlValue::Text(token_digest.as_str().to_owned())],
        )
        .map_err(unavailable)?;
        let Some(row) = rows.rows.first() else {
            let _ = transaction.rollback();
            return Ok(HandoffOpenConsumeOutcomeV1::Concealed);
        };
        let grant_payload = text(&row.values, 0).ok_or_else(codec_unavailable)?;
        let grant: HandoffOpenGrantV1 = decode(grant_payload)?;
        if grant.context() != expected || consumed_at >= *grant.expires_at() {
            let _ = transaction.rollback();
            return Ok(HandoffOpenConsumeOutcomeV1::Concealed);
        }

        let consumed_request_id = optional_text(&row.values, 1).map_err(|_| codec_unavailable())?;
        let consumed_input_digest =
            optional_text(&row.values, 2).map_err(|_| codec_unavailable())?;
        let consumption_payload = optional_text(&row.values, 3).map_err(|_| codec_unavailable())?;
        match (
            consumed_request_id,
            consumed_input_digest,
            consumption_payload,
        ) {
            (Some(stored_request_id), Some(stored_input_digest), Some(payload)) => {
                let consumption: HandoffOpenConsumptionV1 = decode(payload)?;
                let _ = transaction.rollback();
                if stored_request_id == request_id.as_str()
                    && stored_input_digest == input_digest.as_str()
                    && consumption.request_id() == request_id
                    && consumption.input_digest() == input_digest
                {
                    return Ok(HandoffOpenConsumeOutcomeV1::Consumed(consumption));
                }
                return Ok(HandoffOpenConsumeOutcomeV1::Concealed);
            }
            (None, None, None) => {}
            _ => return Err(codec_unavailable()),
        }

        let consumption = grant
            .consume(request_id.clone(), input_digest.clone(), consumed_at)
            .map_err(|_| codec_unavailable())?;
        let payload = encode(&consumption)?;
        execute_tx(
            &transaction,
            "UPDATE handoff_open_grants_v1
             SET consumed_request_id = ?2,
                 consumed_input_digest = ?3,
                 consumption_payload = ?4
             WHERE token_digest = ?1
               AND consumption_payload IS NULL",
            vec![
                ExactSqlValue::Text(token_digest.as_str().to_owned()),
                ExactSqlValue::Text(request_id.as_str().to_owned()),
                ExactSqlValue::Text(input_digest.as_str().to_owned()),
                ExactSqlValue::Text(payload),
            ],
        )
        .map_err(unavailable)?;
        transaction
            .commit()
            .map(|_| HandoffOpenConsumeOutcomeV1::Consumed(consumption))
            .map_err(unavailable)
    }
}

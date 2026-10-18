//! Durable single-use handoff opens on the canonical registered SQL channel.
//!
//! This authority stores only secret-free token digests and bindings. It uses
//! the registered exact SQL handle; it never opens a database or creates a
//! parallel authority.

use std::time::Duration;

use tracedecay_contracts::{
    HandoffOpenAuthorityError, HandoffOpenAuthorityPort, HandoffOpenConsumeOutcomeV1,
    HandoffOpenConsumptionV1, HandoffOpenExpectationV1, HandoffOpenGrantV1,
    HandoffOpenListFilterV1, HandoffOpenListingV1, RequestId,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};

use crate::exact_sql::{
    ExactSqlError, ExactSqlHandle, ExactSqlJobTransaction, ExactSqlRows, ExactSqlValue,
    decode_json, encode_json, optional_text_at, text_column,
};
use crate::repository::RetainedExactSqlCapability;
use crate::workflow::statement;

pub const HANDOFF_OPEN_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS handoff_open_grants_v1 (
    token_digest TEXT NOT NULL PRIMARY KEY,
    issued_request_id TEXT NOT NULL UNIQUE,
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
    retained: RetainedExactSqlCapability,
}

impl HandoffOpenSqliteAuthority {
    pub async fn from_retained_exact_sql(
        retained: RetainedExactSqlCapability,
    ) -> Result<Self, HandoffOpenSqliteAuthorityBuildError> {
        let authority = Self { retained };
        require_handoff_open_schema(authority.handle()).await?;
        Ok(authority)
    }

    fn handle(&self) -> &ExactSqlHandle {
        self.retained.handle()
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

async fn query_handle(
    handle: &ExactSqlHandle,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, ExactSqlError> {
    handle
        .query_async(statement(sql, params)?, Duration::from_secs(5))
        .await
}

async fn require_handoff_open_schema(
    handle: &ExactSqlHandle,
) -> Result<(), HandoffOpenSqliteAuthorityBuildError> {
    let rows = query_handle(
        handle,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        vec![ExactSqlValue::Text("handoff_open_grants_v1".to_owned())],
    )
    .await
    .map_err(|_| HandoffOpenSqliteAuthorityBuildError::Unavailable)?;
    if rows.rows.len() == 1 {
        Ok(())
    } else {
        Err(HandoffOpenSqliteAuthorityBuildError::Unavailable)
    }
}

impl HandoffOpenAuthorityPort for HandoffOpenSqliteAuthority {
    async fn issue(
        &self,
        grant: &HandoffOpenGrantV1,
    ) -> Result<HandoffOpenGrantV1, HandoffOpenAuthorityError> {
        let payload = encode_json(grant, |_| codec_unavailable())?;
        let grant = grant.clone();
        self.handle()
            .transact(move |transaction| issue_grant(transaction, &grant, &payload))
            .await
            .map_err(unavailable)?
    }

    async fn list(
        &self,
        filter: &HandoffOpenListFilterV1,
        limit: u32,
    ) -> Result<Vec<HandoffOpenListingV1>, HandoffOpenAuthorityError> {
        // The recipient/session/scope match lives inside the grant payload, so
        // it cannot be pushed into SQL without denormalizing secret-adjacent
        // binding fields into indexed columns. Ordering and the ceiling are
        // pushed down; the match is applied after decoding. `limit + 1` rows
        // are asked for so the caller can be told a ceiling was reached rather
        // than being handed a silently short frontier.
        //
        // Expired grants are deliberately NOT filtered out here: a lapsed
        // handoff is a reportable outcome, and `resolve`'s expiry check exists
        // to refuse redemption, not to erase history.
        let ceiling = i64::from(limit).saturating_add(1);
        let rows = query_handle(
            self.handle(),
            "SELECT grant_payload, consumption_payload
             FROM handoff_open_grants_v1
             ORDER BY issued_at DESC, token_digest ASC
             LIMIT ?1",
            vec![ExactSqlValue::Integer(ceiling)],
        )
        .await
        .map_err(unavailable)?;

        let mut listings = Vec::new();
        for row in &rows.rows {
            let payload = text_column(&row.values, 0).ok_or_else(codec_unavailable)?;
            let grant: HandoffOpenGrantV1 = decode_json(payload, |_| codec_unavailable())?;
            if !filter.matches(grant.context()) {
                continue;
            }
            let consumed_at =
                match optional_text_at(&row.values, 1).map_err(|_| codec_unavailable())? {
                    Some(payload) => {
                        let consumption: HandoffOpenConsumptionV1 =
                            decode_json(payload, |_| codec_unavailable())?;
                        Some(*consumption.consumed_at())
                    }
                    None => None,
                };
            listings.push(HandoffOpenListingV1 { grant, consumed_at });
            if listings.len() as u32 >= limit {
                break;
            }
        }
        Ok(listings)
    }

    async fn resolve(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenExpectationV1,
        observed_at: UtcMicros,
    ) -> Result<Option<HandoffOpenGrantV1>, HandoffOpenAuthorityError> {
        let rows = query_handle(
            self.handle(),
            "SELECT grant_payload FROM handoff_open_grants_v1 WHERE token_digest = ?1",
            vec![ExactSqlValue::Text(token_digest.as_str().to_owned())],
        )
        .await
        .map_err(unavailable)?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let payload = text_column(&row.values, 0).ok_or_else(codec_unavailable)?;
        let grant: HandoffOpenGrantV1 = decode_json(payload, |_| codec_unavailable())?;
        if !expected.matches(grant.context()) || observed_at >= *grant.expires_at() {
            return Ok(None);
        }
        Ok(Some(grant))
    }

    async fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenExpectationV1,
        request_id: &RequestId,
        input_digest: &ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<HandoffOpenConsumeOutcomeV1, HandoffOpenAuthorityError> {
        let consumption = ConsumeRequest {
            token_digest: token_digest.clone(),
            expected: expected.clone(),
            request_id: request_id.clone(),
            input_digest: input_digest.clone(),
            consumed_at,
        };
        self.handle()
            .transact(move |transaction| consume_grant(transaction, &consumption))
            .await
            .map_err(unavailable)?
    }
}

fn job_query(
    transaction: &ExactSqlJobTransaction<'_>,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, HandoffOpenAuthorityError> {
    transaction
        .query(statement(sql, params).map_err(unavailable)?)
        .map_err(unavailable)
}

fn job_execute(
    transaction: &ExactSqlJobTransaction<'_>,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<(), HandoffOpenAuthorityError> {
    transaction
        .execute(statement(sql, params).map_err(unavailable)?)
        .map(|_| ())
        .map_err(unavailable)
}

fn issue_grant(
    transaction: &ExactSqlJobTransaction<'_>,
    grant: &HandoffOpenGrantV1,
    payload: &str,
) -> Result<HandoffOpenGrantV1, HandoffOpenAuthorityError> {
    let existing = job_query(
        transaction,
        "SELECT grant_payload FROM handoff_open_grants_v1
         WHERE token_digest = ?1 OR issued_request_id = ?2",
        vec![
            ExactSqlValue::Text(grant.token_digest().as_str().to_owned()),
            ExactSqlValue::Text(grant.issued_request_id().as_str().to_owned()),
        ],
    )?;
    if let Some(row) = existing.rows.first() {
        let persisted_payload = text_column(&row.values, 0).ok_or_else(codec_unavailable)?;
        let persisted: HandoffOpenGrantV1 =
            decode_json(persisted_payload, |_| codec_unavailable())?;
        if persisted.same_issue_identity(grant) {
            return Ok(persisted);
        }
        return Err(HandoffOpenAuthorityError::Conflict);
    }
    job_execute(
        transaction,
        "INSERT INTO handoff_open_grants_v1 (
             token_digest, issued_request_id, grant_payload, issued_at, expires_at,
             consumed_request_id, consumed_input_digest, consumption_payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL)",
        vec![
            ExactSqlValue::Text(grant.token_digest().as_str().to_owned()),
            ExactSqlValue::Text(grant.issued_request_id().as_str().to_owned()),
            ExactSqlValue::Text(payload.to_owned()),
            ExactSqlValue::Integer(grant.issued_at().0),
            ExactSqlValue::Integer(grant.expires_at().0),
        ],
    )?;
    Ok(grant.clone())
}

struct ConsumeRequest {
    token_digest: ManifestDigest,
    expected: HandoffOpenExpectationV1,
    request_id: RequestId,
    input_digest: ManifestDigest,
    consumed_at: UtcMicros,
}

fn consume_grant(
    transaction: &ExactSqlJobTransaction<'_>,
    request: &ConsumeRequest,
) -> Result<HandoffOpenConsumeOutcomeV1, HandoffOpenAuthorityError> {
    let rows = job_query(
        transaction,
        "SELECT grant_payload, consumed_request_id, consumed_input_digest,
                consumption_payload
         FROM handoff_open_grants_v1
         WHERE token_digest = ?1",
        vec![ExactSqlValue::Text(
            request.token_digest.as_str().to_owned(),
        )],
    )?;
    let Some(row) = rows.rows.first() else {
        return Ok(HandoffOpenConsumeOutcomeV1::Concealed);
    };
    let grant_payload = text_column(&row.values, 0).ok_or_else(codec_unavailable)?;
    let grant: HandoffOpenGrantV1 = decode_json(grant_payload, |_| codec_unavailable())?;
    if !request.expected.matches(grant.context()) || request.consumed_at >= *grant.expires_at() {
        return Ok(HandoffOpenConsumeOutcomeV1::Concealed);
    }

    let consumed_request_id = optional_text_at(&row.values, 1).map_err(|_| codec_unavailable())?;
    let consumed_input_digest =
        optional_text_at(&row.values, 2).map_err(|_| codec_unavailable())?;
    let consumption_payload = optional_text_at(&row.values, 3).map_err(|_| codec_unavailable())?;
    match (
        consumed_request_id,
        consumed_input_digest,
        consumption_payload,
    ) {
        (Some(stored_request_id), Some(stored_input_digest), Some(payload)) => {
            let consumption: HandoffOpenConsumptionV1 =
                decode_json(payload, |_| codec_unavailable())?;
            if stored_request_id != request.request_id.as_str() {
                return Ok(HandoffOpenConsumeOutcomeV1::Concealed);
            }
            if stored_input_digest != request.input_digest.as_str() {
                return Err(HandoffOpenAuthorityError::IdempotencyConflict);
            }
            if consumption.request_id() != &request.request_id
                || consumption.input_digest() != &request.input_digest
            {
                return Err(codec_unavailable());
            }
            return Ok(HandoffOpenConsumeOutcomeV1::Consumed(Box::new(consumption)));
        }
        (None, None, None) => {}
        _ => return Err(codec_unavailable()),
    }

    let consumption = grant
        .consume(
            request.request_id.clone(),
            request.input_digest.clone(),
            request.consumed_at,
        )
        .map_err(|_| codec_unavailable())?;
    let payload = encode_json(&consumption, |_| codec_unavailable())?;
    job_execute(
        transaction,
        "UPDATE handoff_open_grants_v1
         SET consumed_request_id = ?2,
             consumed_input_digest = ?3,
             consumption_payload = ?4
         WHERE token_digest = ?1
           AND consumption_payload IS NULL",
        vec![
            ExactSqlValue::Text(request.token_digest.as_str().to_owned()),
            ExactSqlValue::Text(request.request_id.as_str().to_owned()),
            ExactSqlValue::Text(request.input_digest.as_str().to_owned()),
            ExactSqlValue::Text(payload),
        ],
    )?;
    Ok(HandoffOpenConsumeOutcomeV1::Consumed(Box::new(consumption)))
}

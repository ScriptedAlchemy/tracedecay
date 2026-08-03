use serde::{Serialize, de::DeserializeOwned};
use tracedecay_domain::{
    DurableObservationV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, SanitizationReceiptV1,
};
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCoverageV1};

use crate::session_temporal_operations::{
    FrozenPublicationReceipt, SANITIZER_VERSION as SUMMARY_PUBLICATION_SANITIZER_VERSION,
    receipt_id as summary_receipt_id,
};
use crate::{global_db_operation_error, global_db_operation_message};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::triggers::{INVARIANTS, Invariant};
use super::{AUDIT_PAGE_ROWS, OBSERVATION_AUDIT_PAGE_ROWS, OPERATION};

/// How an invariant's optional row `audit_query` participates in validation.
///
/// Selection is name/category-driven (stable `violation` strings), never
/// `INVARIANTS[index]` ordinals, so SESSION_* audits stay included when the
/// catalog order shifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvariantRowAuditCategory {
    /// Cheap identity / session-temporal state audits for every bounded pass.
    Bounded,
    /// Expensive full-table or join-heavy audits reserved for exhaustive passes.
    Expensive,
}

/// Bounded non-exhaustive audits: prior cheap identity checks plus SESSION
/// cursor / refresh / generation / ownership state.
const BOUNDED_ROW_AUDIT_VIOLATIONS: &[&str] = &[
    "graph_scopes contains a store/project identity mismatch",
    "projection_queue contains an observation identity mismatch",
    "observation projection provenance contains invalid message_created",
    "session cursor key rotation state is invalid",
    "session refresh operation state is invalid",
    "session temporal generation state is invalid",
    "session temporal authority ownership is invalid",
];

const OBSERVATION_ROW_AUDIT_VIOLATIONS: &[&str] = &[
    "committed observation references a missing receipt",
    "committed observation contains invalid authority JSON",
];

pub(super) fn observation_row_audit_covers(invariant: &Invariant) -> bool {
    OBSERVATION_ROW_AUDIT_VIOLATIONS.contains(&invariant.violation)
}

fn classify_invariant_row_audit(invariant: &Invariant) -> Option<InvariantRowAuditCategory> {
    invariant.audit_query.as_ref()?;
    if observation_row_audit_covers(invariant) {
        return None;
    }
    if BOUNDED_ROW_AUDIT_VIOLATIONS.contains(&invariant.violation) {
        Some(InvariantRowAuditCategory::Bounded)
    } else {
        // Fail closed: unknown audits run only on exhaustive passes.
        Some(InvariantRowAuditCategory::Expensive)
    }
}

fn bounded_row_audit_invariants() -> impl Iterator<Item = &'static Invariant> {
    INVARIANTS.iter().filter(|invariant| {
        classify_invariant_row_audit(invariant) == Some(InvariantRowAuditCategory::Bounded)
    })
}

pub(super) async fn query_has_rows(
    conn: &impl QueryExecutor,
    query: &str,
) -> tracedecay_runtime_core::errors::Result<bool> {
    // Existence only — but the exact SQL channel materializes a whole
    // result set before handing back the first row, and caps that at
    // MAX_QUERY_ROWS. An audit query matching more violations than the cap
    // therefore failed the entire invariant pass with a materialization-limit
    // error instead of reporting the violation it had just found. Bounding the
    // query to a single row keeps the cost O(1) in violations and makes the
    // answer independent of how large the offending set is.
    let bounded = format!("SELECT 1 FROM ({query}) LIMIT 1");
    let mut rows = conn
        .query(&bounded, ())
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(super) fn authority_violation(
    message: impl Into<String>,
) -> tracedecay_runtime_core::errors::TraceDecayError {
    global_db_operation_message(OPERATION, message)
}

pub(super) fn decode_authority_json<T: DeserializeOwned>(
    json: &str,
    authority: &str,
) -> tracedecay_runtime_core::errors::Result<T> {
    serde_json::from_str(json)
        .map_err(|error| authority_violation(format!("invalid {authority}: {error}")))
}

pub(super) fn encode_authority_json<T: Serialize>(
    value: &T,
    authority: &str,
) -> tracedecay_runtime_core::errors::Result<String> {
    serde_json::to_string(value)
        .map_err(|error| authority_violation(format!("cannot encode {authority}: {error}")))
}

pub(super) async fn validate_receipt_authority_rows(
    conn: &impl QueryExecutor,
    after_rowid: i64,
) -> tracedecay_runtime_core::errors::Result<(i64, i64)> {
    let mut high_water = after_rowid;
    let mut audited = 0;
    loop {
        let mut rows = conn
            .query(
                "SELECT rowid, receipt_id, sanitizer_version, payload_digest, receipt_json
             FROM sanitization_receipts WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
                params![high_water, AUDIT_PAGE_ROWS],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            page_rows += 1;
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
            if sanitizer_version == SUMMARY_PUBLICATION_SANITIZER_VERSION {
                let receipt: FrozenPublicationReceipt = decode_authority_json(
                    &receipt_json,
                    "summary publication receipt authority JSON",
                )?;
                let is_digest = |value: &str| {
                    value.len() == 64
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                };
                if receipt_id != summary_receipt_id(&receipt.summary_id, &payload_digest)
                    || receipt.disposition != "accepted"
                    || receipt.generation <= 0
                    || receipt.published_at <= 0
                    || !is_digest(&payload_digest)
                    || !is_digest(&receipt.publication_manifest_digest)
                    || serde_json::from_str::<serde_json::Value>(&receipt.frozen_watermarks_json)
                        .is_err()
                    || serde_json::from_str::<serde_json::Value>(&receipt.source_horizon_json)
                        .is_err()
                {
                    return Err(authority_violation(
                        "summary publication receipt authority columns disagree with receipt JSON",
                    ));
                }
                audited += 1;
                continue;
            }
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
        drop(rows);
        if page_rows < AUDIT_PAGE_ROWS {
            return Ok((high_water, audited));
        }
    }
}

pub(super) async fn validate_observation_authority_rows(
    conn: &impl QueryExecutor,
    after_sequence: i64,
) -> tracedecay_runtime_core::errors::Result<(i64, i64)> {
    let mut high_water = after_sequence;
    let mut audited = 0;
    loop {
        let mut rows = conn
            .query(
                "SELECT observation.sequence, observation.observation_id,
                    observation.payload_digest, observation.receipt_id,
                    observation.observation_json, observation.committed_cursor_json,
                    receipt.receipt_id
             FROM observations AS observation
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = observation.receipt_id
             WHERE observation.sequence > ?1 ORDER BY observation.sequence LIMIT ?2",
                params![high_water, OBSERVATION_AUDIT_PAGE_ROWS],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            page_rows += 1;
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
            let Some(joined_receipt_id) = row
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
            if sequence <= 0
                || observation.observation_id().as_str() != observation_id
                || observation.payload_reference().digest().as_str() != payload_digest
                || observation.receipt().receipt().receipt_id().as_str() != receipt_id
                || joined_receipt_id != receipt_id
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
        drop(rows);
        if page_rows < OBSERVATION_AUDIT_PAGE_ROWS {
            return Ok((high_water, audited));
        }
    }
}

pub(super) async fn validate_source_cursor_authority_rows(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    let mut cursor_rowid = 0;
    let mut advance_rowid = 0;
    loop {
        let (next_cursor_rowid, next_advance_rowid, complete) =
            validate_source_cursor_authority_chunk(conn, cursor_rowid, advance_rowid).await?;
        cursor_rowid = next_cursor_rowid;
        advance_rowid = next_advance_rowid;
        if complete {
            return Ok(());
        }
    }
}

pub(super) async fn validate_source_cursor_authority_chunk(
    conn: &impl QueryExecutor,
    mut cursor_rowid: i64,
    mut advance_rowid: i64,
) -> tracedecay_runtime_core::errors::Result<(i64, i64, bool)> {
    let mut rows = conn
        .query(
            "SELECT rowid, source_json, scope_json, cursor_json FROM source_cursors
             WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            params![cursor_rowid, AUDIT_PAGE_ROWS],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut page_rows = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        page_rows += 1;
        cursor_rowid = row
            .get::<i64>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let source_json = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let scope_json = row
            .get::<String>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let cursor_json = row
            .get::<String>(3)
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
    if page_rows == AUDIT_PAGE_ROWS {
        return Ok((cursor_rowid, advance_rowid, false));
    }

    let mut rows = conn
        .query(
            "SELECT advance.rowid, advance.source_json, advance.scope_json,
                    advance.coverage_json, advance.reason, advance.receipt_id,
                    receipt.receipt_json
             FROM source_cursor_advances AS advance
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = advance.receipt_id
             WHERE advance.rowid > ?1 ORDER BY advance.rowid LIMIT ?2",
            params![advance_rowid, AUDIT_PAGE_ROWS],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut page_rows = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        page_rows += 1;
        advance_rowid = row
            .get::<i64>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let source_json = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let scope_json = row
            .get::<String>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let coverage_json = row
            .get::<String>(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let reason = row
            .get::<String>(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let receipt_id = row
            .get::<Option<String>>(5)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let receipt_json = row
            .get::<Option<String>>(6)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let source: ObservationSourceIdentityV1 =
            decode_authority_json(&source_json, "source cursor advance identity JSON")?;
        let scope: ObservationScopeV1 =
            decode_authority_json(&scope_json, "source cursor advance scope JSON")?;
        let coverage: ObservationCoverageV1 =
            decode_authority_json(&coverage_json, "source cursor advance coverage JSON")?;
        let receipt_matches = match ObservationCoverageReason::try_from(reason.as_str()) {
            Ok(parsed_reason) => match (receipt_id, receipt_json) {
                (None, None) => parsed_reason.is_receiptless(),
                (Some(receipt_id), Some(receipt_json)) if !parsed_reason.is_receiptless() => {
                    let receipt: SanitizationReceiptV1 = decode_authority_json(
                        &receipt_json,
                        "source cursor advance sanitization receipt JSON",
                    )?;
                    receipt.receipt().receipt_id().as_str() == receipt_id
                        && parsed_reason.disposition_matches(Some(receipt.disposition()))
                        && (parsed_reason == ObservationCoverageReason::DuplicateObservation)
                            == receipt.payload().is_some()
                }
                _ => false,
            },
            Err(_) => false,
        };
        if source_json != encode_authority_json(&source, "source cursor advance identity JSON")?
            || scope_json != encode_authority_json(&scope, "source cursor advance scope JSON")?
            || coverage_json
                != encode_authority_json(&coverage, "source cursor advance coverage JSON")?
            || !receipt_matches
        {
            return Err(authority_violation(
                "source cursor advance contains invalid authority evidence",
            ));
        }
    }
    drop(rows);
    Ok((cursor_rowid, advance_rowid, page_rows < AUDIT_PAGE_ROWS))
}

pub(super) async fn validate_mutable_invariant_rows(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    for invariant in bounded_row_audit_invariants() {
        if let Some(query) = invariant.audit_query
            && query_has_rows(conn, query).await?
        {
            return Err(global_db_operation_message(OPERATION, invariant.violation));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::harness::RegisteredGlobalDbHarness;

    /// Expensive audits that must stay exhaustive-only (explicit classification).
    const EXPENSIVE_ROW_AUDIT_VIOLATIONS: &[&str] = &[
        "observation projection provenance contains a receipt mismatch",
        "workflow projection contains an observation receipt mismatch",
        "observation projection disposition contains a receipt mismatch",
        "observation projection checkpoints contains a negative sequence",
        "projection checkpoint exceeds the committed observation frontier",
        "global database contains a foreign-key violation",
        "session summary authority is mutable or crosses sessions",
        "session temporal receipts or cursor keys are mutable",
    ];

    fn session_temporal_bounded_violations() -> &'static [&'static str] {
        &[
            "session cursor key rotation state is invalid",
            "session refresh operation state is invalid",
            "session temporal generation state is invalid",
            "session temporal authority ownership is invalid",
        ]
    }

    #[test]
    fn bounded_selection_is_name_driven_and_includes_session_temporal_audits() {
        let bounded: Vec<&str> = bounded_row_audit_invariants()
            .map(|invariant| invariant.violation)
            .collect();
        for violation in BOUNDED_ROW_AUDIT_VIOLATIONS {
            assert!(
                bounded.contains(violation),
                "bounded selection missed named audit: {violation}"
            );
        }
        for violation in session_temporal_bounded_violations() {
            assert!(
                bounded.contains(violation),
                "SESSION temporal audit must run on bounded passes: {violation}"
            );
        }
        for violation in EXPENSIVE_ROW_AUDIT_VIOLATIONS {
            assert!(
                !bounded.contains(violation),
                "expensive audit must not run on bounded passes: {violation}"
            );
        }
        // No ordinal dependence: the named set is exactly the filter result.
        assert_eq!(bounded.len(), BOUNDED_ROW_AUDIT_VIOLATIONS.len());
    }

    #[test]
    fn every_row_audit_is_explicitly_classified_bounded_or_expensive() {
        for invariant in INVARIANTS {
            let Some(category) = classify_invariant_row_audit(invariant) else {
                continue;
            };
            let in_bounded = BOUNDED_ROW_AUDIT_VIOLATIONS.contains(&invariant.violation);
            let in_expensive = EXPENSIVE_ROW_AUDIT_VIOLATIONS.contains(&invariant.violation);
            assert!(
                !(in_bounded && in_expensive),
                "audit classified in both lists: {}",
                invariant.violation
            );
            match category {
                InvariantRowAuditCategory::Bounded => {
                    assert!(
                        in_bounded,
                        "bounded category missing from BOUNDED list: {}",
                        invariant.violation
                    );
                }
                InvariantRowAuditCategory::Expensive => {
                    assert!(
                        in_expensive,
                        "expensive/unknown audit must be listed in EXPENSIVE: {}",
                        invariant.violation
                    );
                }
            }
        }
    }

    #[test]
    fn observation_row_validation_replaces_redundant_sql_scans() {
        for violation in OBSERVATION_ROW_AUDIT_VIOLATIONS {
            let invariant = INVARIANTS
                .iter()
                .find(|invariant| invariant.violation == *violation)
                .unwrap();
            assert!(observation_row_audit_covers(invariant));
            assert_eq!(classify_invariant_row_audit(invariant), None);
        }
    }

    async fn open_db() -> RegisteredGlobalDbHarness {
        RegisteredGlobalDbHarness::open("schema-invariant-rows").await
    }

    fn advance_values(range_start: u64) -> String {
        use tracedecay_domain::{
            ObservationOrderingDomainV1, ObservationSourceGenerationV1,
            ObservationSourceIdentityV1, ObservationSourceRangeV1, SessionId,
        };

        let source = serde_json::to_string(
            &ObservationSourceIdentityV1::new(SessionId::new("session.paging").unwrap()).unwrap(),
        )
        .unwrap();
        let scope = serde_json::to_string(&ObservationScopeV1::Profile).unwrap();
        let coverage = serde_json::to_string(&ObservationCoverageV1::new(
            ObservationSourceGenerationV1::new(1).unwrap(),
            ObservationOrderingDomainV1::FileBytes,
            ObservationSourceRangeV1::new(range_start, range_start + 1).unwrap(),
        ))
        .unwrap();
        format!(
            "('{source}', '{scope}', '{coverage}', '{}', NULL)",
            ObservationCoverageReason::BlankFrame.as_str()
        )
    }

    /// A store whose `source_cursor_advances` table outgrew one SQL-channel
    /// page must still be audited end to end. The scan pages by rowid, so the
    /// audit has to keep reading past the first page — both to finish clean and
    /// to see a violation that lives beyond it.
    #[tokio::test]
    async fn source_cursor_advance_audit_reads_past_the_first_page() {
        let harness = open_db().await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin advance paging fixture transaction");
        let conn = &transaction;

        let page_spanning_rows = usize::try_from(AUDIT_PAGE_ROWS).unwrap() + 1;
        let values = (0..page_spanning_rows)
            .map(|index| advance_values(u64::try_from(index).unwrap() * 2))
            .collect::<Vec<_>>()
            .join(",\n");
        conn.execute_batch(&format!(
            "INSERT INTO source_cursor_advances(
                source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES {values};"
        ))
        .await
        .expect("seed page-spanning cursor advances");

        validate_source_cursor_authority_rows(conn)
            .await
            .expect("a page-spanning advance table must audit clean");

        // The violation lands on the second page, so a single-page audit would
        // report success.
        conn.execute_batch(
            "INSERT INTO source_cursor_advances(
                source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES ('{}', '{}', 'not-json', 'blank_frame', NULL);",
        )
        .await
        .expect("seed corrupt advance beyond the first page");
        let error = validate_source_cursor_authority_rows(conn)
            .await
            .expect_err("corruption beyond the first page must be reported");
        assert!(
            error.to_string().contains("source cursor advance"),
            "unexpected advance audit error: {error}"
        );
    }

    #[tokio::test]
    async fn source_cursor_advance_audit_resumes_from_durable_seek_position() {
        let harness = open_db().await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin resumable advance audit fixture transaction");
        let conn = &transaction;
        let page_spanning_rows = usize::try_from(AUDIT_PAGE_ROWS).unwrap() + 1;
        let values = (0..page_spanning_rows)
            .map(|index| advance_values(u64::try_from(index).unwrap() * 2))
            .collect::<Vec<_>>()
            .join(",\n");
        conn.execute_batch(&format!(
            "INSERT INTO source_cursor_advances(
                source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES {values};"
        ))
        .await
        .expect("seed resumable cursor advances");

        let (cursor_rowid, advance_rowid, complete) =
            validate_source_cursor_authority_chunk(conn, 0, 0)
                .await
                .expect("audit first source authority chunk");
        assert!(!complete);
        assert_eq!(cursor_rowid, 0);
        assert_eq!(advance_rowid, AUDIT_PAGE_ROWS);

        let (_, final_advance_rowid, complete) =
            validate_source_cursor_authority_chunk(conn, cursor_rowid, advance_rowid)
                .await
                .expect("resume source authority audit");
        assert!(complete);
        assert_eq!(
            final_advance_rowid,
            i64::try_from(page_spanning_rows).unwrap()
        );
    }

    async fn assert_bounded_and_exhaustive_reject(conn: &impl QueryExecutor, violation: &str) {
        let bounded = validate_mutable_invariant_rows(conn)
            .await
            .expect_err("bounded validation must reject corruption");
        assert!(
            bounded.to_string().contains(violation),
            "bounded error missing `{violation}`: {bounded}"
        );
        let exhaustive = super::super::validate_invariant_rows(conn)
            .await
            .expect_err("exhaustive validation must reject corruption");
        assert!(
            exhaustive.to_string().contains(violation),
            "exhaustive error missing `{violation}`: {exhaustive}"
        );
    }

    /// `is_fresh` skips the row audits on the creating open, but a reopen
    /// (`is_fresh = false`) still runs the exhaustive audit and rejects
    /// corruption — the freshness fast path must be invisible on reopen.
    #[tokio::test]
    async fn fresh_open_skips_row_audits_but_reopen_audits_exhaustively() {
        let harness = open_db().await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin invariant fixture transaction");
        let conn = &transaction;
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
             DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
             DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;
             INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES
                ('cursor-a', 1, X'01', 100, NULL),
                ('cursor-b', 2, X'02', 200, NULL);",
        )
        .await
        .expect("seed corrupt cursor keys");

        // Fresh creation skips the row audits, so the seeded corruption is not
        // scanned even with force_exhaustive requested.
        super::super::ensure_authority_invariants(conn, true, true)
            .await
            .expect("fresh creation skips the row audits");

        // A reopen audits exhaustively and rejects the same corruption.
        let error = super::super::ensure_authority_invariants(conn, true, false)
            .await
            .expect_err("reopen must run the exhaustive audit");
        assert!(
            error
                .to_string()
                .contains("session cursor key rotation state is invalid"),
            "reopen audit missed corruption: {error}"
        );
    }

    #[tokio::test]
    async fn bounded_and_exhaustive_reject_corrupt_session_cursor_keys() {
        let harness = open_db().await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin invariant fixture transaction");
        let conn = &transaction;
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
             DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
             DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;
             INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES
                ('cursor-a', 1, X'01', 100, NULL),
                ('cursor-b', 2, X'02', 200, NULL);",
        )
        .await
        .expect("seed corrupt cursor keys");
        assert_bounded_and_exhaustive_reject(conn, "session cursor key rotation state is invalid")
            .await;
    }

    #[tokio::test]
    async fn bounded_and_exhaustive_reject_corrupt_session_refresh_rows() {
        let harness = open_db().await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin invariant fixture transaction");
        let conn = &transaction;
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS session_refresh_operations_insert_guard_v1;
             DROP TRIGGER IF EXISTS session_refresh_operations_state_guard_v1;
             INSERT INTO session_refresh_operations (
                session_id, operation_id, request_digest, target_frontier_json,
                state, created_at, updated_at, terminal_at, failure_code
             ) VALUES (
                'session-refresh', 'op-1', 'digest-1',
                '{\"observed_through\":1,\"committed_through\":0}',
                'running', 200, 100, NULL, NULL
             );",
        )
        .await
        .expect("seed corrupt refresh operation");
        assert_bounded_and_exhaustive_reject(conn, "session refresh operation state is invalid")
            .await;
    }

    #[tokio::test]
    async fn bounded_and_exhaustive_reject_corrupt_session_generation_rows() {
        let harness = open_db().await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin invariant fixture transaction");
        let conn = &transaction;
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS session_temporal_generations_insert_guard_v1;
             DROP TRIGGER IF EXISTS session_temporal_generations_state_guard_v1;
             DROP TRIGGER IF EXISTS session_temporal_generations_single_active_insert_v1;
             DROP TRIGGER IF EXISTS session_temporal_generations_single_active_update_v1;
             DROP INDEX IF EXISTS idx_session_temporal_generations_one_active;
             INSERT INTO session_temporal_generations (
                session_id, generation, state, frozen_watermarks_json,
                created_at, ready_at, activated_at, completed_at
             ) VALUES
                ('session-gen', 1, 'active', '{}', 1, 2, 3, NULL),
                ('session-gen', 2, 'active', '{}', 4, 5, 6, NULL);",
        )
        .await
        .expect("seed corrupt generations");
        assert_bounded_and_exhaustive_reject(conn, "session temporal generation state is invalid")
            .await;
    }

    #[tokio::test]
    async fn bounded_and_exhaustive_reject_corrupt_session_ownership_rows() {
        let harness = open_db().await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin invariant fixture transaction");
        let conn = &transaction;
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS session_summary_availability_owner_insert_v1;
             INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES ('summary-anchor', '{}', '{}', 'test-generation');
             INSERT INTO session_temporal_generations (
                session_id, generation, state, frozen_watermarks_json,
                created_at, ready_at, activated_at, completed_at
             ) VALUES ('availability-session', 1, 'building', '{}', 1, NULL, NULL, NULL);
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-owned-elsewhere', 'summary-session', 'summary-anchor',
                'summary', 'summary', '{}', NULL, 1
             );
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'availability-session', 1, 'summary-owned-elsewhere',
                'available', '{}', NULL, 2
             );",
        )
        .await
        .expect("seed corrupt session ownership");
        assert_bounded_and_exhaustive_reject(
            conn,
            "session temporal authority ownership is invalid",
        )
        .await;
    }

    mod authority_cross_checks {
        //! The indexed columns beside each authority JSON blob are redundant
        //! by construction: they exist so the daemon can filter and join
        //! without decoding. Nothing at write time forces them to agree with
        //! the JSON, so if they ever drift the daemon serves rows whose keys
        //! contradict their own authority. These pin the detection.

        use tracedecay_domain::{ObservationScopeV1, ObservationSourceRangeV1, ProjectId};
        use tracedecay_store::observation::{ObservationCoverageReason, ObservationCoverageV1};

        use super::super::super::test_fixture::{
            authority_fixture, open_registered, seed_observation, shift,
        };
        use super::super::{
            validate_observation_authority_rows, validate_receipt_authority_rows,
            validate_source_cursor_authority_rows,
        };
        use tracedecay_runtime_core::db::engine::{Executor, params};

        /// The digest column is what payload lookups key on. A row whose
        /// column names one payload while its JSON names another resolves to
        /// the wrong payload without ever failing a query.
        #[tokio::test]
        async fn observation_authority_columns_are_cross_checked_against_json() {
            let (_directory, conn) = open_registered().await;
            let (observation, cursor) = authority_fixture(0, "observation-columns");
            let receipt = observation.receipt();
            conn.execute(
                "INSERT INTO sanitization_receipts
                 (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    receipt.receipt().receipt_id().as_str(),
                    receipt.receipt().sanitizer_version().as_str(),
                    observation.payload_reference().digest().as_str(),
                    serde_json::to_string(receipt).unwrap()
                ],
            )
            .await
            .expect("seed sanitization receipt");
            conn.execute(
                "INSERT INTO observations
                 (observation_id, payload_digest, receipt_id, observation_json,
                  committed_cursor_json)
                 VALUES (?1, 'digest.disagrees-with-json', ?2, ?3, ?4)",
                params![
                    observation.observation_id().as_str(),
                    receipt.receipt().receipt_id().as_str(),
                    serde_json::to_string(&observation).unwrap(),
                    serde_json::to_string(&cursor).unwrap()
                ],
            )
            .await
            .expect("seed observation with a drifted digest column");

            let error = validate_observation_authority_rows(&conn, 0)
                .await
                .expect_err("a drifted observation column must be detected");

            assert!(
                error.to_string().contains(
                    "committed observation authority columns disagree with observation JSON"
                ),
                "{error}"
            );
        }

        /// The committed cursor travels beside the observation as the resume
        /// point for its source. If it disagrees with the observation's own
        /// range, resuming from it replays or skips the difference.
        #[tokio::test]
        async fn committed_cursor_is_cross_checked_against_observation_evidence() {
            let (_directory, conn) = open_registered().await;
            let (observation, cursor) = authority_fixture(0, "committed-cursor");
            let receipt = observation.receipt();
            conn.execute(
                "INSERT INTO sanitization_receipts
                 (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    receipt.receipt().receipt_id().as_str(),
                    receipt.receipt().sanitizer_version().as_str(),
                    observation.payload_reference().digest().as_str(),
                    serde_json::to_string(receipt).unwrap()
                ],
            )
            .await
            .expect("seed sanitization receipt");
            conn.execute(
                "INSERT INTO observations
                 (observation_id, payload_digest, receipt_id, observation_json,
                  committed_cursor_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    observation.observation_id().as_str(),
                    observation.payload_reference().digest().as_str(),
                    receipt.receipt().receipt_id().as_str(),
                    serde_json::to_string(&observation).unwrap(),
                    serde_json::to_string(&shift(&cursor, 25)).unwrap()
                ],
            )
            .await
            .expect("seed observation with a drifted committed cursor");

            let error = validate_observation_authority_rows(&conn, 0)
                .await
                .expect_err("a committed cursor past its observation must be detected");

            assert!(
                error
                    .to_string()
                    .contains("committed source cursor disagrees with observation source evidence"),
                "{error}"
            );
        }

        /// The sanitizer version column selects which sanitizer's guarantees a
        /// payload carries. A column that outruns the receipt JSON silently
        /// credits the payload with a contract it was never sanitized under.
        #[tokio::test]
        async fn receipt_authority_columns_are_cross_checked_against_json() {
            let (_directory, conn) = open_registered().await;
            let (observation, _) = authority_fixture(0, "receipt-columns");
            let receipt = observation.receipt();
            conn.execute(
                "INSERT INTO sanitization_receipts
                 (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES (?1, 'sanitizer.disagrees-with-json', ?2, ?3)",
                params![
                    receipt.receipt().receipt_id().as_str(),
                    observation.payload_reference().digest().as_str(),
                    serde_json::to_string(receipt).unwrap()
                ],
            )
            .await
            .expect("seed receipt with a drifted sanitizer version column");

            let error = validate_receipt_authority_rows(&conn, 0)
                .await
                .expect_err("a drifted receipt column must be detected");

            assert!(
                error
                    .to_string()
                    .contains("sanitization receipt authority columns disagree with receipt JSON"),
                "{error}"
            );
        }

        /// Cursor rows are looked up by their `(source_json, scope_json)` key.
        /// A key naming a different scope than the cursor it stores hands the
        /// wrong source its resume point.
        #[tokio::test]
        async fn source_cursor_authority_keys_are_cross_checked_against_json() {
            let (_directory, conn) = open_registered().await;
            let (_, cursor) = seed_observation(&conn, 0, "cursor-keys").await;
            let foreign_scope = ObservationScopeV1::Project {
                project_id: ProjectId::new("project.cursor-keys").expect("project identifier"),
            };
            conn.execute(
                "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
                 VALUES (?1, ?2, ?3)",
                params![
                    serde_json::to_string(cursor.source()).unwrap(),
                    serde_json::to_string(&foreign_scope).unwrap(),
                    serde_json::to_string(&cursor).unwrap()
                ],
            )
            .await
            .expect("seed cursor whose scope key disagrees with its JSON");

            let error = validate_source_cursor_authority_rows(&conn)
                .await
                .expect_err("a drifted cursor key must be detected");

            assert!(
                error
                    .to_string()
                    .contains("source cursor authority keys disagree with cursor JSON"),
                "{error}"
            );
        }

        /// An advance is what lets a frontier legitimately run past the last
        /// commit. A receipt-bearing reason with no receipt row is exactly the
        /// shape that would launder unreceipted progress into durable coverage.
        #[tokio::test]
        async fn source_cursor_advance_authority_is_cross_checked() {
            let (_directory, conn) = open_registered().await;
            let (_, cursor) = seed_observation(&conn, 0, "advance-evidence").await;
            let coverage = ObservationCoverageV1::new(
                cursor.generation(),
                cursor.ordering_domain(),
                ObservationSourceRangeV1::new(cursor.position(), cursor.position() + 50).unwrap(),
            );
            conn.execute(
                "INSERT INTO source_cursor_advances
                 (source_json, scope_json, coverage_json, reason)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    serde_json::to_string(cursor.source()).unwrap(),
                    serde_json::to_string(cursor.scope()).unwrap(),
                    serde_json::to_string(&coverage).unwrap(),
                    ObservationCoverageReason::DuplicateObservation.as_str()
                ],
            )
            .await
            .expect("seed advance claiming a receipt it does not have");

            let error = validate_source_cursor_authority_rows(&conn)
                .await
                .expect_err("a receiptless receipt-bearing advance must be detected");

            assert!(
                error
                    .to_string()
                    .contains("source cursor advance contains invalid authority evidence"),
                "{error}"
            );
        }
    }
}

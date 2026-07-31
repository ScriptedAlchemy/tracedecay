//! Durable SQLite authority, fencing, and publication for Remote Brain recovery.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::{OperationBudgetUsage, ResolvedScope};
use tracedecay_application::remote::auth::{
    RemoteAuthenticationError, RemoteAuthorityAuthenticationPort,
    RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentAuthorityErrorV1,
    RemoteEnrollmentAuthorityPortV1, RemoteEnrollmentCommitReceiptV1,
    RemoteEnrollmentCredentialLookupPortV1,
};
use tracedecay_application::remote::capture::{
    RemoteCapturePersistenceErrorV1, RemoteWriterAuthorityV1,
};
use tracedecay_application::remote::replay::{
    RemoteReplayApplicationErrorV1, RemoteReplayCurrentWriterPortV1, RemoteReplayCurrentWriterV1,
    RemoteReplayFrameV1, RemoteReplayPolicyDecisionV1, RemoteReplayPolicyEvidencePortV1,
    RemoteReplayPolicyEvidenceV1, RemoteReplayPolicyPortV1,
};
use tracedecay_application::remote::query::{
    RemoteExactObservationQueryErrorV1, RemoteQueryAuthorizationEvidenceV1,
    RemoteQueryAuthorizationPortV1, RemoteQueryPolicyRecordV1,
};
use tracedecay_domain::{
    BrainId, BrainNodeId, CanonicalObservationIdV1, CurrentRemoteAuthorityStateV1,
    CurrentRemoteAuthorityV1, EnrollmentCredentialRecordV1, EnrollmentCredentialStateV1,
    EnrollmentGrantV1, EntityId, RemoteAuthorityUnavailableReasonV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, UtcMicros, canonical_sha256,
};
use tracedecay_store::{AuthorityCasV1, ShardWatermarkV1, StoreRuntimeBindingV1};

use crate::migration_sql::{
    MigrationSqlError, MigrationSqlHandle, MigrationSqlRows, MigrationSqlStatement,
    MigrationSqlTransaction, MigrationSqlValue,
};
use crate::remote_spool::RemoteAuthorityReachabilityPortV1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS remote_authority_v1 (
    shard_key TEXT PRIMARY KEY,
    writer_json TEXT NOT NULL,
    binding_json TEXT NOT NULL,
    placement_revision INTEGER NOT NULL CHECK (placement_revision > 0),
    frontier_json TEXT,
    serving INTEGER NOT NULL CHECK (serving IN (0, 1)),
    old_writer_json TEXT,
    old_authority_read_only INTEGER NOT NULL CHECK (old_authority_read_only IN (0, 1))
);
CREATE TABLE IF NOT EXISTS remote_fence_sink_v1 (
    shard_key TEXT NOT NULL,
    sink_id TEXT NOT NULL,
    writer_json TEXT NOT NULL,
    binding_json TEXT NOT NULL,
    placement_revision INTEGER NOT NULL CHECK (placement_revision > 0),
    PRIMARY KEY (shard_key, sink_id)
);
CREATE TABLE IF NOT EXISTS remote_publication_v1 (
    shard_key TEXT PRIMARY KEY,
    writer_json TEXT NOT NULL,
    binding_json TEXT NOT NULL,
    placement_revision INTEGER NOT NULL CHECK (placement_revision > 0),
    frontier_json TEXT NOT NULL
);
"#;

const REMOTE_ENROLLMENT_SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS remote_enrollment_grants_v1 (
    grant_id TEXT PRIMARY KEY,
    grant_revision INTEGER NOT NULL CHECK (grant_revision > 0),
    grant_json TEXT NOT NULL,
    admission_json TEXT NOT NULL,
    consumed_at INTEGER,
    enrollment_id TEXT
) STRICT;
CREATE TABLE IF NOT EXISTS remote_enrollment_credentials_v1 (
    enrollment_id TEXT PRIMARY KEY,
    credential_revision INTEGER NOT NULL CHECK (credential_revision > 0),
    enrollment_json TEXT NOT NULL,
    commit_receipt_json TEXT
) STRICT;
"#;

const REMOTE_REPLAY_POLICY_SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS remote_replay_policy_v1 (
    scope_digest TEXT PRIMARY KEY,
    policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
    evidence_json TEXT NOT NULL
) STRICT;
"#;

const REMOTE_QUERY_POLICY_SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS remote_query_policy_v1 (
    scope_digest TEXT PRIMARY KEY,
    policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
    record_json TEXT NOT NULL
) STRICT;
"#;

#[derive(Clone)]
pub struct RegisteredRemoteEnrollmentAuthorityV1 {
    handle: MigrationSqlHandle,
}

impl RegisteredRemoteEnrollmentAuthorityV1 {
    pub fn from_registered(
        handle: MigrationSqlHandle,
    ) -> Result<Self, RemoteEnrollmentAuthorityErrorV1> {
        handle
            .execute_batch(REMOTE_ENROLLMENT_SCHEMA_V1.to_owned())
            .map_err(enrollment_unavailable)?;
        Ok(Self { handle })
    }

    /// Trusted local administration provisions an already validated,
    /// fingerprint-only grant. Inbound protocol requests never call this path.
    pub fn provision_grant(
        &self,
        grant: &EnrollmentGrantV1,
        admission: &RemoteEnrollmentAdmissionEvidenceV1,
    ) -> Result<(), RemoteEnrollmentAuthorityErrorV1> {
        grant
            .validate()
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        admission
            .validate_for(grant)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let encoded = serde_json::to_string(grant)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let admission_json = serde_json::to_string(admission)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let revision = i64::try_from(grant.revision)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(enrollment_unavailable)?;
        let existing = enrollment_query_tx(
            &transaction,
            "SELECT grant_json, admission_json FROM remote_enrollment_grants_v1 WHERE grant_id = ?1",
            vec![MigrationSqlValue::Text(grant.grant_id.as_str().to_owned())],
        )?;
        if let Some(row) = existing.rows.first() {
            return if enrollment_text(&row.values, 0) == Some(encoded.as_str())
                && enrollment_text(&row.values, 1) == Some(admission_json.as_str())
            {
                transaction
                    .commit()
                    .map_err(enrollment_unavailable)
                    .map(|_| ())
            } else {
                Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
            };
        }
        enrollment_execute_tx(
            &transaction,
            "INSERT INTO remote_enrollment_grants_v1 (
                grant_id, grant_revision, grant_json, admission_json, consumed_at, enrollment_id
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL)",
            vec![
                MigrationSqlValue::Text(grant.grant_id.as_str().to_owned()),
                MigrationSqlValue::Integer(revision),
                MigrationSqlValue::Text(encoded),
                MigrationSqlValue::Text(admission_json),
            ],
        )?;
        transaction
            .commit()
            .map_err(enrollment_unavailable)
            .map(|_| ())
    }

    pub fn load_commit_receipt(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1> {
        let rows = self
            .handle
            .query(
                enrollment_statement(
                    "SELECT commit_receipt_json FROM remote_enrollment_credentials_v1
                     WHERE enrollment_id = ?1",
                    vec![MigrationSqlValue::Text(enrollment_id.as_str().to_owned())],
                )?,
                Duration::from_secs(5),
            )
            .map_err(enrollment_unavailable)?;
        let encoded = rows
            .rows
            .first()
            .and_then(|row| enrollment_text(&row.values, 0))
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
        let receipt: RemoteEnrollmentCommitReceiptV1 = serde_json::from_str(encoded)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        receipt
            .validate()
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        Ok(receipt)
    }
}

impl RemoteEnrollmentAuthorityPortV1 for RegisteredRemoteEnrollmentAuthorityV1 {
    fn load_grant(
        &self,
        grant_id: &EntityId,
    ) -> Result<EnrollmentGrantV1, RemoteEnrollmentAuthorityErrorV1> {
        let rows = self
            .handle
            .query(
                enrollment_statement(
                    "SELECT grant_json, consumed_at FROM remote_enrollment_grants_v1
                     WHERE grant_id = ?1",
                    vec![MigrationSqlValue::Text(grant_id.as_str().to_owned())],
                )?,
                Duration::from_secs(5),
            )
            .map_err(enrollment_unavailable)?;
        let row = rows
            .rows
            .first()
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
        if !matches!(row.values.get(1), Some(MigrationSqlValue::Null)) {
            return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
        }
        let encoded = enrollment_text(&row.values, 0)
            .ok_or(RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        serde_json::from_str(encoded)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
    }

    fn load_admission_evidence(
        &self,
        grant_id: &EntityId,
    ) -> Result<RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentAuthorityErrorV1> {
        let rows = self
            .handle
            .query(
                enrollment_statement(
                    "SELECT admission_json, consumed_at FROM remote_enrollment_grants_v1
                     WHERE grant_id = ?1",
                    vec![MigrationSqlValue::Text(grant_id.as_str().to_owned())],
                )?,
                Duration::from_secs(5),
            )
            .map_err(enrollment_unavailable)?;
        let row = rows
            .rows
            .first()
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
        if !matches!(row.values.get(1), Some(MigrationSqlValue::Null)) {
            return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
        }
        let encoded = enrollment_text(&row.values, 0)
            .ok_or(RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        serde_json::from_str(encoded)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
    }

    fn commit_enrollment(
        &self,
        grant: &EnrollmentGrantV1,
        enrollment: &EnrollmentCredentialRecordV1,
        input_digest: &tracedecay_domain::ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1> {
        grant
            .validate()
            .and_then(|()| enrollment.validate())
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        if grant.brain_id != enrollment.brain_id
            || grant.node_id != enrollment.node_id
            || grant.scope != enrollment.scope
            || !enrollment.capabilities.is_subset(&grant.capabilities)
            || enrollment.revision != grant.revision
            || enrollment.issued_at != consumed_at
            || enrollment.expires_at > grant.expires_at
        {
            return Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict);
        }
        let grant_json = serde_json::to_string(grant)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let enrollment_json = serde_json::to_string(enrollment)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let revision = i64::try_from(enrollment.revision)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let transaction_started = Instant::now();
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(enrollment_unavailable)?;
        let persisted = enrollment_query_tx(
            &transaction,
            "SELECT admission_json FROM remote_enrollment_grants_v1
             WHERE grant_id = ?1 AND grant_revision = ?2
               AND grant_json = ?3 AND consumed_at IS NULL",
            vec![
                MigrationSqlValue::Text(grant.grant_id.as_str().to_owned()),
                MigrationSqlValue::Integer(
                    i64::try_from(grant.revision)
                        .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?,
                ),
                MigrationSqlValue::Text(grant_json.clone()),
            ],
        )?;
        let admission_json = persisted
            .rows
            .first()
            .and_then(|row| enrollment_text(&row.values, 0))
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantConsumed)?;
        let admission: RemoteEnrollmentAdmissionEvidenceV1 =
            serde_json::from_str(admission_json)
                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        admission
            .validate_for(grant)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        if admission.effective_deadline().is_elapsed_at(consumed_at) {
            return Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict);
        }
        let updated = transaction
            .execute(enrollment_statement(
                "UPDATE remote_enrollment_grants_v1
                 SET consumed_at = ?1, enrollment_id = ?2
                 WHERE grant_id = ?3 AND grant_revision = ?4
                   AND grant_json = ?5 AND consumed_at IS NULL",
                vec![
                    MigrationSqlValue::Integer(consumed_at.0),
                    MigrationSqlValue::Text(enrollment.enrollment_id.as_str().to_owned()),
                    MigrationSqlValue::Text(grant.grant_id.as_str().to_owned()),
                    MigrationSqlValue::Integer(
                        i64::try_from(grant.revision)
                            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?,
                    ),
                    MigrationSqlValue::Text(grant_json.clone()),
                ],
            )?)
            .map_err(enrollment_unavailable)?;
        if updated.changed_rows != 1 {
            return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
        }
        let bytes_consumed = u64::try_from(
            grant_json.len()
                + admission_json.len()
                + enrollment_json.len()
                + input_digest.as_str().len(),
        )
        .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let inserted = transaction
            .execute(enrollment_statement(
                "INSERT INTO remote_enrollment_credentials_v1 (
                    enrollment_id, credential_revision, enrollment_json, commit_receipt_json
                 ) VALUES (?1, ?2, ?3, NULL)",
                vec![
                    MigrationSqlValue::Text(enrollment.enrollment_id.as_str().to_owned()),
                    MigrationSqlValue::Integer(revision),
                    MigrationSqlValue::Text(enrollment_json),
                ],
            )?)
            .map_err(enrollment_unavailable)?;
        if inserted.changed_rows != 1 {
            return Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict);
        }
        let receipt = RemoteEnrollmentCommitReceiptV1 {
            admission,
            prior_grant_digest: tracedecay_domain::canonical_sha256(grant)
                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?,
            input_digest: input_digest.clone(),
            committed_state_digest: tracedecay_domain::canonical_sha256(enrollment)
                .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?,
            consumed_at,
            budget: OperationBudgetUsage {
                units_consumed: u64::try_from(updated.changed_rows)
                    .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?
                    .saturating_add(
                        u64::try_from(inserted.changed_rows)
                            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?,
                    ),
                bytes_consumed,
                elapsed_micros: u64::try_from(transaction_started.elapsed().as_micros())
                    .unwrap_or(u64::MAX),
            },
            enrollment: enrollment.clone(),
        };
        receipt
            .validate()
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let receipt_json = serde_json::to_string(&receipt)
            .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
        let receipt_written = transaction
            .execute(enrollment_statement(
                "UPDATE remote_enrollment_credentials_v1
                 SET commit_receipt_json = ?2
                 WHERE enrollment_id = ?1 AND commit_receipt_json IS NULL",
                vec![
                    MigrationSqlValue::Text(enrollment.enrollment_id.as_str().to_owned()),
                    MigrationSqlValue::Text(receipt_json),
                ],
            )?)
            .map_err(enrollment_unavailable)?;
        if receipt_written.changed_rows != 1 {
            return Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict);
        }
        transaction.commit().map_err(enrollment_unavailable)?;
        Ok(receipt)
    }
}

impl RemoteEnrollmentCredentialLookupPortV1 for RegisteredRemoteEnrollmentAuthorityV1 {
    fn enrollment_by_id(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        self.query_enrollment(
            "SELECT enrollment_json FROM remote_enrollment_credentials_v1
             WHERE enrollment_id = ?1",
            vec![MigrationSqlValue::Text(enrollment_id.as_str().to_owned())],
        )
    }

    fn authority_enrollment(
        &self,
        brain_id: &BrainId,
        node_id: &BrainNodeId,
        revision: u64,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        let candidates = self.query_enrollments(
            "SELECT enrollment_json FROM remote_enrollment_credentials_v1
             WHERE credential_revision = ?1",
            vec![MigrationSqlValue::Integer(
                i64::try_from(revision)
                    .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?,
            )],
        )?;
        let mut matches = candidates.into_iter().filter(|enrollment| {
            enrollment.brain_id == *brain_id && enrollment.node_id == *node_id
        });
        let enrollment = matches
            .next()
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantNotFound)?;
        if matches.next().is_some() {
            return Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict);
        }
        Ok(enrollment)
    }

    fn enrollment_commit_receipt(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1> {
        self.load_commit_receipt(enrollment_id)
    }
}

impl RemoteAuthorityAuthenticationPort for RegisteredRemoteEnrollmentAuthorityV1 {
    fn authenticate_connected_authority(
        &self,
        expected_authority: &CurrentRemoteAuthorityV1,
        expected_credential: &EnrollmentCredentialRecordV1,
        observed_at: UtcMicros,
    ) -> Result<(), RemoteAuthenticationError> {
        let persisted = self
            .enrollment_by_id(&expected_credential.enrollment_id)
            .map_err(|_| RemoteAuthenticationError::AuthorityAuthenticationFailed)?;
        if persisted != *expected_credential
            || persisted.brain_id != expected_authority.fence.brain_id
            || persisted.node_id != expected_authority.fence.authority_node_id
            || persisted.revision != expected_authority.credential_revision
            || persisted.state_at(observed_at) != EnrollmentCredentialStateV1::Active
        {
            return Err(RemoteAuthenticationError::InvalidAuthorityCredential);
        }
        Ok(())
    }
}

impl RegisteredRemoteEnrollmentAuthorityV1 {
    fn query_enrollment(
        &self,
        sql: &str,
        params: Vec<MigrationSqlValue>,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        self.query_enrollments(sql, params)?
            .into_iter()
            .next()
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantNotFound)
    }

    fn query_enrollments(
        &self,
        sql: &str,
        params: Vec<MigrationSqlValue>,
    ) -> Result<Vec<EnrollmentCredentialRecordV1>, RemoteEnrollmentAuthorityErrorV1> {
        let rows = self
            .handle
            .query(enrollment_statement(sql, params)?, Duration::from_secs(5))
            .map_err(enrollment_unavailable)?;
        rows.rows
            .iter()
            .map(|row| {
                let encoded = enrollment_text(&row.values, 0)
                    .ok_or(RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
                let enrollment: EnrollmentCredentialRecordV1 = serde_json::from_str(encoded)
                    .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
                enrollment
                    .validate()
                    .map_err(|_| RemoteEnrollmentAuthorityErrorV1::IdentityConflict)?;
                Ok(enrollment)
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct RegisteredRemoteReplayPolicyAuthorityV1 {
    handle: MigrationSqlHandle,
}

impl RegisteredRemoteReplayPolicyAuthorityV1 {
    pub fn from_registered(
        handle: MigrationSqlHandle,
    ) -> Result<Self, RemoteReplayApplicationErrorV1> {
        handle
            .execute_batch(REMOTE_REPLAY_POLICY_SCHEMA_V1.to_owned())
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?;
        Ok(Self { handle })
    }

    pub fn provision(
        &self,
        evidence: &RemoteReplayPolicyEvidenceV1,
    ) -> Result<(), RemoteReplayApplicationErrorV1> {
        evidence.validate()?;
        let scope_digest = canonical_sha256(&(
            "tracedecay.remote-replay-policy-scope.v1",
            &evidence.repository_scope,
        ))
        .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        let revision = i64::try_from(evidence.policy_revision)
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        let encoded = serde_json::to_string(evidence)
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        self.handle
            .execute(
                enrollment_statement(
                    "INSERT INTO remote_replay_policy_v1 (
                            scope_digest, policy_revision, evidence_json
                         ) VALUES (?1, ?2, ?3)
                         ON CONFLICT(scope_digest) DO UPDATE SET
                            policy_revision = excluded.policy_revision,
                            evidence_json = excluded.evidence_json
                         WHERE excluded.policy_revision > remote_replay_policy_v1.policy_revision
                            OR (
                                excluded.policy_revision = remote_replay_policy_v1.policy_revision
                                AND excluded.evidence_json = remote_replay_policy_v1.evidence_json
                            )",
                    vec![
                        MigrationSqlValue::Text(scope_digest.as_str().to_owned()),
                        MigrationSqlValue::Integer(revision),
                        MigrationSqlValue::Text(encoded.clone()),
                    ],
                )
                .map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?,
            )
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?;
        let persisted = self.load(&evidence.repository_scope)?;
        if persisted != *evidence {
            return Err(RemoteReplayApplicationErrorV1::PolicyMismatch);
        }
        Ok(())
    }

    pub fn policy_for_scope(
        &self,
        scope: &RemoteRepositoryScopeV1,
    ) -> Result<RemoteReplayPolicyEvidenceV1, RemoteReplayApplicationErrorV1> {
        self.load(scope)
    }

    fn load(
        &self,
        scope: &RemoteRepositoryScopeV1,
    ) -> Result<RemoteReplayPolicyEvidenceV1, RemoteReplayApplicationErrorV1> {
        let scope_digest = canonical_sha256(&("tracedecay.remote-replay-policy-scope.v1", scope))
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        let rows = self
            .handle
            .query(
                enrollment_statement(
                    "SELECT evidence_json FROM remote_replay_policy_v1 WHERE scope_digest = ?1",
                    vec![MigrationSqlValue::Text(scope_digest.as_str().to_owned())],
                )
                .map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?,
                Duration::from_secs(5),
            )
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?;
        let encoded = rows
            .rows
            .first()
            .and_then(|row| enrollment_text(&row.values, 0))
            .ok_or(RemoteReplayApplicationErrorV1::PolicyUnavailable)?;
        let evidence: RemoteReplayPolicyEvidenceV1 = serde_json::from_str(encoded)
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        evidence.validate()?;
        Ok(evidence)
    }
}

impl RemoteReplayPolicyPortV1 for RegisteredRemoteReplayPolicyAuthorityV1 {
    fn authorize_current_policy(
        &self,
        frame: &RemoteReplayFrameV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteReplayPolicyDecisionV1, RemoteReplayApplicationErrorV1> {
        let evidence = self.load(&frame.capture.writer.scope)?;
        evidence.validate_for(frame)?;
        if evidence.revalidated_at > observed_at {
            return Err(RemoteReplayApplicationErrorV1::PolicyMismatch);
        }
        Ok(evidence.decision)
    }
}

impl RemoteReplayPolicyEvidencePortV1 for RegisteredRemoteReplayPolicyAuthorityV1 {
    fn current_policy_evidence(
        &self,
        frame: &RemoteReplayFrameV1,
    ) -> Result<RemoteReplayPolicyEvidenceV1, RemoteReplayApplicationErrorV1> {
        let evidence = self.load(&frame.capture.writer.scope)?;
        evidence.validate_for(frame)?;
        Ok(evidence)
    }
}

#[derive(Clone)]
pub struct RegisteredRemoteQueryPolicyAuthorityV1 {
    handle: MigrationSqlHandle,
}

impl RegisteredRemoteQueryPolicyAuthorityV1 {
    pub fn from_registered(
        handle: MigrationSqlHandle,
    ) -> Result<Self, RemoteExactObservationQueryErrorV1> {
        handle
            .execute_batch(REMOTE_QUERY_POLICY_SCHEMA_V1.to_owned())
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        Ok(Self { handle })
    }

    pub fn provision(
        &self,
        record: &RemoteQueryPolicyRecordV1,
    ) -> Result<(), RemoteExactObservationQueryErrorV1> {
        record.validate()?;
        let scope_digest = canonical_sha256(&(
            "tracedecay.remote-query-policy-scope.v1",
            &record.repository_scope,
        ))
        .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        let revision = i64::try_from(record.policy_revision)
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        let encoded = serde_json::to_string(record)
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        self.handle
            .execute(
                enrollment_statement(
                    "INSERT INTO remote_query_policy_v1 (
                            scope_digest, policy_revision, record_json
                         ) VALUES (?1, ?2, ?3)
                         ON CONFLICT(scope_digest) DO UPDATE SET
                            policy_revision = excluded.policy_revision,
                            record_json = excluded.record_json
                         WHERE excluded.policy_revision > remote_query_policy_v1.policy_revision
                            OR (
                                excluded.policy_revision = remote_query_policy_v1.policy_revision
                                AND excluded.record_json = remote_query_policy_v1.record_json
                            )",
                    vec![
                        MigrationSqlValue::Text(scope_digest.as_str().to_owned()),
                        MigrationSqlValue::Integer(revision),
                        MigrationSqlValue::Text(encoded),
                    ],
                )
                .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?,
            )
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        if self.load(&record.repository_scope)? != *record {
            return Err(RemoteExactObservationQueryErrorV1::PolicyUnavailable);
        }
        Ok(())
    }

    fn load(
        &self,
        repository_scope: &RemoteRepositoryScopeV1,
    ) -> Result<RemoteQueryPolicyRecordV1, RemoteExactObservationQueryErrorV1> {
        let scope_digest = canonical_sha256(&(
            "tracedecay.remote-query-policy-scope.v1",
            repository_scope,
        ))
        .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        let rows = self
            .handle
            .query(
                enrollment_statement(
                    "SELECT record_json FROM remote_query_policy_v1 WHERE scope_digest = ?1",
                    vec![MigrationSqlValue::Text(scope_digest.as_str().to_owned())],
                )
                .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?,
                Duration::from_secs(5),
            )
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        let encoded = rows
            .rows
            .first()
            .and_then(|row| enrollment_text(&row.values, 0))
            .ok_or(RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        let record: RemoteQueryPolicyRecordV1 = serde_json::from_str(encoded)
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        record.validate()?;
        Ok(record)
    }
}

impl RemoteQueryAuthorizationPortV1 for RegisteredRemoteQueryPolicyAuthorityV1 {
    fn authorize(
        &self,
        scope: &ResolvedScope,
        repository_scope: &RemoteRepositoryScopeV1,
        observation_id: &CanonicalObservationIdV1,
        expected_authority: &RemoteWriterFenceV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteQueryAuthorizationEvidenceV1, RemoteExactObservationQueryErrorV1> {
        let record = self.load(repository_scope)?;
        if record.scope != *scope || record.revalidated_at > observed_at {
            return Err(RemoteExactObservationQueryErrorV1::PolicyUnavailable);
        }
        let evidence = RemoteQueryAuthorizationEvidenceV1 {
            repository_scope: repository_scope.clone(),
            observation_id: observation_id.clone(),
            expected_authority: expected_authority.clone(),
            policy_revision: record.policy_revision,
            decision: record.decision,
            authority: record.authority,
            revalidated_at: record.revalidated_at,
        };
        evidence.validate_for(
            scope,
            repository_scope,
            observation_id,
            expected_authority,
            observed_at,
        )?;
        Ok(evidence)
    }
}

fn enrollment_statement(
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlStatement, RemoteEnrollmentAuthorityErrorV1> {
    MigrationSqlStatement::new(sql.to_owned(), params).map_err(enrollment_unavailable)
}

fn enrollment_query_tx(
    transaction: &MigrationSqlTransaction,
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlRows, RemoteEnrollmentAuthorityErrorV1> {
    transaction
        .query(enrollment_statement(sql, params)?)
        .map_err(enrollment_unavailable)
}

fn enrollment_execute_tx(
    transaction: &MigrationSqlTransaction,
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<(), RemoteEnrollmentAuthorityErrorV1> {
    transaction
        .execute(enrollment_statement(sql, params)?)
        .map(|_| ())
        .map_err(enrollment_unavailable)
}

fn enrollment_text(values: &[MigrationSqlValue], index: usize) -> Option<&str> {
    match values.get(index)? {
        MigrationSqlValue::Text(value) => Some(value),
        _ => None,
    }
}

fn enrollment_unavailable(_: MigrationSqlError) -> RemoteEnrollmentAuthorityErrorV1 {
    RemoteEnrollmentAuthorityErrorV1::Unavailable
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAuthorityCasCommitV1 {
    pub previous_writer: RemoteWriterAuthorityV1,
    pub installed_writer: RemoteWriterAuthorityV1,
    pub previous_binding: StoreRuntimeBindingV1,
    pub installed_binding: StoreRuntimeBindingV1,
    pub installed_placement_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemotePublicationReceiptV1 {
    pub writer: RemoteWriterAuthorityV1,
    pub binding: StoreRuntimeBindingV1,
    pub placement_revision: u64,
    pub frontier: ShardWatermarkV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteQueryAuthoritySnapshotV1 {
    pub project_id: tracedecay_domain::ProjectId,
    pub scope: RemoteRepositoryScopeV1,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub placement_revision: u64,
    pub binding: StoreRuntimeBindingV1,
    pub frontier: Option<ShardWatermarkV1>,
    pub observed_at: UtcMicros,
}

pub struct RusqliteRemoteAuthorityStoreV1 {
    connection: Mutex<Connection>,
}

impl RusqliteRemoteAuthorityStoreV1 {
    pub fn open_in_memory() -> Result<Self, RemoteAuthorityStorageErrorV1> {
        let connection = Connection::open_in_memory().map_err(storage)?;
        Self::from_connection(connection)
    }

    pub fn from_connection(connection: Connection) -> Result<Self, RemoteAuthorityStorageErrorV1> {
        connection.execute_batch(SCHEMA).map_err(storage)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn query_authority_snapshot(
        &self,
        project_id: &tracedecay_domain::ProjectId,
        scope: &RemoteRepositoryScopeV1,
        expected_fence: &tracedecay_domain::RemoteWriterFenceV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteQueryAuthoritySnapshotV1, RemoteAuthorityStorageErrorV1> {
        scope
            .validate()
            .map_err(|_| RemoteAuthorityStorageErrorV1::InvalidContract)?;
        expected_fence
            .validate()
            .map_err(|_| RemoteAuthorityStorageErrorV1::InvalidContract)?;
        if project_id != &scope.project_id {
            return Err(RemoteAuthorityStorageErrorV1::InvalidContract);
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RemoteAuthorityStorageErrorV1::Unavailable)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage)?;
        let mut statement = transaction
            .prepare("SELECT shard_key FROM remote_authority_v1")
            .map_err(storage)?;
        let keys = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        drop(statement);
        for key in keys {
            let stored = read_authority(&transaction, &key)?
                .ok_or(RemoteAuthorityStorageErrorV1::Corruption)?;
            if stored.writer.project_id == *project_id
                && stored.writer.scope == *scope
                && stored.writer.authority.fence.brain_id == expected_fence.brain_id
                && stored.writer.authority.fence.shard_id == expected_fence.shard_id
            {
                validate_authority_binding(
                    &stored.writer,
                    &stored.binding,
                    stored.placement_revision,
                )
                .map_err(|_| RemoteAuthorityStorageErrorV1::Corruption)?;
                if let Some(frontier) = &stored.frontier {
                    validate_frontier(&stored.binding, frontier)
                        .map_err(|_| RemoteAuthorityStorageErrorV1::Corruption)?;
                }
                let state = if !stored.serving
                    || stored.frontier.is_none()
                    || stored.old_writer.is_some() && !stored.old_authority_read_only
                    || !publication_matches(&transaction, &key, &stored)?
                {
                    CurrentRemoteAuthorityStateV1::Partial {
                        known_fence: Some(stored.writer.authority.fence.clone()),
                        missing: std::collections::BTreeSet::from([
                            RemoteAuthorityUnavailableReasonV1::FenceUnverified,
                        ]),
                        observed_at,
                    }
                } else {
                    CurrentRemoteAuthorityStateV1::Available(CurrentRemoteAuthorityV1 {
                        observed_at,
                        ..stored.writer.authority.clone()
                    })
                };
                let snapshot = RemoteQueryAuthoritySnapshotV1 {
                    project_id: stored.writer.project_id,
                    scope: stored.writer.scope,
                    authority: state,
                    placement_revision: stored.placement_revision,
                    binding: stored.binding,
                    frontier: stored.frontier,
                    observed_at,
                };
                transaction.commit().map_err(storage)?;
                return Ok(snapshot);
            }
        }
        transaction.commit().map_err(storage)?;
        Err(RemoteAuthorityStorageErrorV1::Unavailable)
    }

    pub fn initialize_authority(
        &self,
        writer: &RemoteWriterAuthorityV1,
        binding: &StoreRuntimeBindingV1,
        placement_revision: u64,
        frontier: &ShardWatermarkV1,
    ) -> Result<(), RemoteAuthorityStorageErrorV1> {
        validate_authority_binding(writer, binding, placement_revision)?;
        validate_frontier(binding, frontier)?;
        let shard_key = encode(&binding.shard_id)?;
        let changed = self
            .connection
            .lock()
            .map_err(|_| RemoteAuthorityStorageErrorV1::Unavailable)?
            .execute(
                "INSERT OR IGNORE INTO remote_authority_v1 (
                    shard_key, writer_json, binding_json, placement_revision,
                    frontier_json, serving, old_writer_json, old_authority_read_only
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 0, NULL, 0)",
                params![
                    shard_key,
                    encode(writer)?,
                    encode(binding)?,
                    sqlite_u64(placement_revision)?,
                    encode(frontier)?,
                ],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(RemoteAuthorityStorageErrorV1::AlreadyInitialized);
        }
        Ok(())
    }

    pub fn compare_and_swap(
        &self,
        cas: &AuthorityCasV1,
        expected_writer: &RemoteWriterAuthorityV1,
        replacement_writer: &RemoteWriterAuthorityV1,
    ) -> Result<RemoteAuthorityCasCommitV1, RemoteAuthorityStorageErrorV1> {
        cas.validate()
            .map_err(|_| RemoteAuthorityStorageErrorV1::InvalidContract)?;
        validate_authority_binding(
            expected_writer,
            &cas.expected_binding,
            cas.expected_placement_revision,
        )?;
        validate_authority_binding(
            replacement_writer,
            &cas.replacement_binding,
            cas.replacement_placement_revision,
        )?;
        if expected_writer.project_id != replacement_writer.project_id
            || expected_writer.scope != replacement_writer.scope
            || expected_writer.authority.fence.brain_id
                != replacement_writer.authority.fence.brain_id
            || expected_writer.authority.fence.shard_id
                != replacement_writer.authority.fence.shard_id
            || expected_writer.authority.fence.generation_id
                != replacement_writer.authority.fence.generation_id
            || expected_writer.authority.fence.placement_revision
                == replacement_writer.authority.fence.placement_revision
            || replacement_writer.authority.fence.authority_epoch
                <= expected_writer.authority.fence.authority_epoch
        {
            return Err(RemoteAuthorityStorageErrorV1::InvalidContract);
        }

        let shard_key = encode(&cas.shard_id)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RemoteAuthorityStorageErrorV1::Unavailable)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let current = read_authority(&transaction, &shard_key)?
            .ok_or(RemoteAuthorityStorageErrorV1::CasConflict)?;
        if current.writer != *expected_writer
            || current.binding != cas.expected_binding
            || current.placement_revision != cas.expected_placement_revision
        {
            return Err(RemoteAuthorityStorageErrorV1::CasConflict);
        }
        let changed = transaction
            .execute(
                "UPDATE remote_authority_v1
                 SET writer_json = ?2, binding_json = ?3, placement_revision = ?4,
                     frontier_json = NULL, serving = 0, old_writer_json = ?5,
                     old_authority_read_only = 0
                 WHERE shard_key = ?1
                   AND writer_json = ?6
                   AND binding_json = ?7
                   AND placement_revision = ?8",
                params![
                    shard_key,
                    encode(replacement_writer)?,
                    encode(&cas.replacement_binding)?,
                    sqlite_u64(cas.replacement_placement_revision)?,
                    encode(expected_writer)?,
                    encode(expected_writer)?,
                    encode(&cas.expected_binding)?,
                    sqlite_u64(cas.expected_placement_revision)?,
                ],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(RemoteAuthorityStorageErrorV1::CasConflict);
        }
        transaction.commit().map_err(storage)?;
        Ok(RemoteAuthorityCasCommitV1 {
            previous_writer: expected_writer.clone(),
            installed_writer: replacement_writer.clone(),
            previous_binding: cas.expected_binding.clone(),
            installed_binding: cas.replacement_binding.clone(),
            installed_placement_revision: cas.replacement_placement_revision,
        })
    }

    pub fn install_fence(
        &self,
        sink_id: &str,
        writer: &RemoteWriterAuthorityV1,
        binding: &StoreRuntimeBindingV1,
        placement_revision: u64,
    ) -> Result<(), RemoteAuthorityStorageErrorV1> {
        validate_sink_id(sink_id)?;
        validate_authority_binding(writer, binding, placement_revision)?;
        let shard_key = encode(&binding.shard_id)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RemoteAuthorityStorageErrorV1::Unavailable)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let current = read_authority(&transaction, &shard_key)?
            .ok_or(RemoteAuthorityStorageErrorV1::AuthorityUnavailable)?;
        if current.writer != *writer
            || current.binding != *binding
            || current.placement_revision != placement_revision
        {
            return Err(RemoteAuthorityStorageErrorV1::StaleFence);
        }
        if let Some(installed) = read_fence(&transaction, &shard_key, sink_id)?
            && installed != (writer.clone(), binding.clone(), placement_revision)
        {
            if installed.0.authority.fence.authority_epoch >= writer.authority.fence.authority_epoch
            {
                return Err(RemoteAuthorityStorageErrorV1::StaleFence);
            }
        }
        transaction
            .execute(
                "INSERT INTO remote_fence_sink_v1 (
                    shard_key, sink_id, writer_json, binding_json, placement_revision
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(shard_key, sink_id) DO UPDATE SET
                    writer_json = excluded.writer_json,
                    binding_json = excluded.binding_json,
                    placement_revision = excluded.placement_revision",
                params![
                    shard_key,
                    sink_id,
                    encode(writer)?,
                    encode(binding)?,
                    sqlite_u64(placement_revision)?,
                ],
            )
            .map_err(storage)?;
        transaction.commit().map_err(storage)
    }

    pub fn fence_old_authority_read_only(
        &self,
        old_writer: &RemoteWriterAuthorityV1,
        replacement_writer: &RemoteWriterAuthorityV1,
        replacement_binding: &StoreRuntimeBindingV1,
    ) -> Result<(), RemoteAuthorityStorageErrorV1> {
        if replacement_writer.authority.fence.brain_id != replacement_binding.shard_id.brain_id
            || replacement_writer.authority.fence.authority_epoch.0
                != replacement_binding.authority_epoch.get()
        {
            return Err(RemoteAuthorityStorageErrorV1::InvalidContract);
        }
        let shard_key = encode(&replacement_binding.shard_id)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| RemoteAuthorityStorageErrorV1::Unavailable)?;
        let changed = connection
            .execute(
                "UPDATE remote_authority_v1
                 SET old_authority_read_only = 1
                 WHERE shard_key = ?1 AND writer_json = ?2 AND old_writer_json = ?3",
                params![shard_key, encode(replacement_writer)?, encode(old_writer)?,],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(RemoteAuthorityStorageErrorV1::CasConflict);
        }
        Ok(())
    }

    pub fn publish_and_enable_serving(
        &self,
        binding: &StoreRuntimeBindingV1,
        writer: &RemoteWriterAuthorityV1,
        placement_revision: u64,
        frontier: &ShardWatermarkV1,
        required_sink_ids: &[&str],
    ) -> Result<RemotePublicationReceiptV1, RemoteAuthorityStorageErrorV1> {
        validate_authority_binding(writer, binding, placement_revision)?;
        validate_frontier(binding, frontier)?;
        if required_sink_ids.is_empty() {
            return Err(RemoteAuthorityStorageErrorV1::InvalidContract);
        }
        let shard_key = encode(&binding.shard_id)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RemoteAuthorityStorageErrorV1::Unavailable)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let current = read_authority(&transaction, &shard_key)?
            .ok_or(RemoteAuthorityStorageErrorV1::AuthorityUnavailable)?;
        if current.writer != *writer
            || current.binding != *binding
            || current.placement_revision != placement_revision
        {
            return Err(RemoteAuthorityStorageErrorV1::StaleFence);
        }
        for sink_id in required_sink_ids {
            validate_sink_id(sink_id)?;
            let Some(installed) = read_fence(&transaction, &shard_key, sink_id)? else {
                return Err(RemoteAuthorityStorageErrorV1::MissingFence {
                    sink_id: (*sink_id).to_owned(),
                });
            };
            if installed != (writer.clone(), binding.clone(), placement_revision) {
                return Err(RemoteAuthorityStorageErrorV1::StaleFence);
            }
        }
        if current.old_writer.is_some() && !current.old_authority_read_only {
            return Err(RemoteAuthorityStorageErrorV1::OldAuthorityStillWritable);
        }
        transaction
            .execute(
                "INSERT INTO remote_publication_v1 (
                    shard_key, writer_json, binding_json, placement_revision, frontier_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(shard_key) DO UPDATE SET
                    writer_json = excluded.writer_json,
                    binding_json = excluded.binding_json,
                    placement_revision = excluded.placement_revision,
                    frontier_json = excluded.frontier_json",
                params![
                    shard_key,
                    encode(writer)?,
                    encode(binding)?,
                    sqlite_u64(placement_revision)?,
                    encode(frontier)?,
                ],
            )
            .map_err(storage)?;
        let changed = transaction
            .execute(
                "UPDATE remote_authority_v1
                 SET frontier_json = ?2, serving = 1
                 WHERE shard_key = ?1
                   AND writer_json = ?3
                   AND binding_json = ?4
                   AND placement_revision = ?5
                   AND (old_writer_json IS NULL OR old_authority_read_only = 1)",
                params![
                    shard_key,
                    encode(frontier)?,
                    encode(writer)?,
                    encode(binding)?,
                    sqlite_u64(placement_revision)?,
                ],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(RemoteAuthorityStorageErrorV1::StaleFence);
        }
        transaction.commit().map_err(storage)?;
        Ok(RemotePublicationReceiptV1 {
            writer: writer.clone(),
            binding: binding.clone(),
            placement_revision,
            frontier: frontier.clone(),
        })
    }
}

impl RemoteAuthorityReachabilityPortV1 for RusqliteRemoteAuthorityStoreV1 {
    fn current_writer_authority(
        &self,
        requested: &RemoteWriterAuthorityV1,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1> {
        requested
            .validate()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Unavailable)?;
        let mut statement = connection
            .prepare("SELECT shard_key FROM remote_authority_v1")
            .map_err(|_| RemoteCapturePersistenceErrorV1::Unavailable)?;
        let shard_keys = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| RemoteCapturePersistenceErrorV1::Unavailable)?;
        let shard_keys = shard_keys
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Unavailable)?;
        drop(statement);
        for shard_key in shard_keys {
            let stored = read_authority(&connection, &shard_key)
                .map_err(capture_storage)?
                .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
            if same_remote_target(&stored.writer, requested) {
                if !stored.serving
                    || stored.frontier.is_none()
                    || stored.old_writer.is_some() && !stored.old_authority_read_only
                    || !publication_matches(&connection, &shard_key, &stored)
                        .map_err(capture_storage)?
                {
                    return Ok(CurrentRemoteAuthorityStateV1::Partial {
                        known_fence: Some(stored.writer.authority.fence),
                        missing: std::collections::BTreeSet::from([
                            RemoteAuthorityUnavailableReasonV1::FenceUnverified,
                        ]),
                        observed_at: stored.writer.authority.observed_at,
                    });
                }
                return Ok(CurrentRemoteAuthorityStateV1::Available(
                    stored.writer.authority,
                ));
            }
        }
        Ok(CurrentRemoteAuthorityStateV1::Unavailable {
            reason: RemoteAuthorityUnavailableReasonV1::RegistryUnavailable,
            observed_at: requested.authority.observed_at,
        })
    }
}

impl RemoteReplayCurrentWriterPortV1 for RusqliteRemoteAuthorityStoreV1 {
    fn current_writer(
        &self,
        frame: &RemoteReplayFrameV1,
    ) -> Result<RemoteReplayCurrentWriterV1, RemoteCapturePersistenceErrorV1> {
        frame
            .validate()
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let state = self.current_writer_authority(&frame.capture.writer)?;
        let writer = match &state {
            CurrentRemoteAuthorityStateV1::Available(authority) => Some(RemoteWriterAuthorityV1 {
                authority: authority.clone(),
                project_id: frame.capture.writer.project_id.clone(),
                scope: frame.capture.writer.scope.clone(),
            }),
            CurrentRemoteAuthorityStateV1::Partial { .. }
            | CurrentRemoteAuthorityStateV1::Unavailable { .. } => None,
        };
        Ok(RemoteReplayCurrentWriterV1 { writer, state })
    }
}

#[derive(Deserialize)]
struct StoredAuthorityV1 {
    writer: RemoteWriterAuthorityV1,
    binding: StoreRuntimeBindingV1,
    placement_revision: u64,
    frontier: Option<ShardWatermarkV1>,
    serving: bool,
    old_writer: Option<RemoteWriterAuthorityV1>,
    old_authority_read_only: bool,
}

fn read_authority(
    connection: &Connection,
    shard_key: &str,
) -> Result<Option<StoredAuthorityV1>, RemoteAuthorityStorageErrorV1> {
    connection
        .query_row(
            "SELECT writer_json, binding_json, placement_revision, frontier_json,
                    serving, old_writer_json, old_authority_read_only
             FROM remote_authority_v1 WHERE shard_key = ?1",
            [shard_key],
            |row| {
                let writer_json: String = row.get(0)?;
                let binding_json: String = row.get(1)?;
                let placement_revision: i64 = row.get(2)?;
                let frontier_json: Option<String> = row.get(3)?;
                let serving: bool = row.get(4)?;
                let old_writer_json: Option<String> = row.get(5)?;
                let old_authority_read_only: bool = row.get(6)?;
                Ok((
                    writer_json,
                    binding_json,
                    placement_revision,
                    frontier_json,
                    serving,
                    old_writer_json,
                    old_authority_read_only,
                ))
            },
        )
        .optional()
        .map_err(storage)?
        .map(
            |(
                writer,
                binding,
                placement,
                frontier,
                serving,
                old_writer,
                old_authority_read_only,
            )| {
                let stored = StoredAuthorityV1 {
                    writer: decode(&writer)?,
                    binding: decode(&binding)?,
                    placement_revision: decode_u64(placement)?,
                    frontier: frontier.as_deref().map(decode).transpose()?,
                    serving,
                    old_writer: old_writer.as_deref().map(decode).transpose()?,
                    old_authority_read_only,
                };
                validate_authority_binding(
                    &stored.writer,
                    &stored.binding,
                    stored.placement_revision,
                )?;
                if let Some(frontier) = &stored.frontier {
                    validate_frontier(&stored.binding, frontier)?;
                }
                Ok(stored)
            },
        )
        .transpose()
}

type StoredFenceV1 = (RemoteWriterAuthorityV1, StoreRuntimeBindingV1, u64);

fn read_fence(
    connection: &Connection,
    shard_key: &str,
    sink_id: &str,
) -> Result<Option<StoredFenceV1>, RemoteAuthorityStorageErrorV1> {
    connection
        .query_row(
            "SELECT writer_json, binding_json, placement_revision
             FROM remote_fence_sink_v1 WHERE shard_key = ?1 AND sink_id = ?2",
            params![shard_key, sink_id],
            |row| {
                let writer_json: String = row.get(0)?;
                let binding_json: String = row.get(1)?;
                let placement_revision: i64 = row.get(2)?;
                Ok((writer_json, binding_json, placement_revision))
            },
        )
        .optional()
        .map_err(storage)?
        .map(|(writer, binding, placement)| {
            Ok((decode(&writer)?, decode(&binding)?, decode_u64(placement)?))
        })
        .transpose()
}

fn validate_authority_binding(
    writer: &RemoteWriterAuthorityV1,
    binding: &StoreRuntimeBindingV1,
    placement_revision: u64,
) -> Result<(), RemoteAuthorityStorageErrorV1> {
    writer
        .validate()
        .map_err(|_| RemoteAuthorityStorageErrorV1::InvalidContract)?;
    if placement_revision == 0
        || !binding.shard_id.is_mutable()
        || writer.authority.fence.brain_id != binding.shard_id.brain_id
        || writer.authority.fence.authority_epoch.0 != binding.authority_epoch.get()
        || writer.authority.fence.placement_revision.get() != placement_revision
    {
        return Err(RemoteAuthorityStorageErrorV1::InvalidContract);
    }
    Ok(())
}

fn publication_matches(
    connection: &Connection,
    shard_key: &str,
    authority: &StoredAuthorityV1,
) -> Result<bool, RemoteAuthorityStorageErrorV1> {
    let publication = connection
        .query_row(
            "SELECT writer_json, binding_json, placement_revision, frontier_json
             FROM remote_publication_v1 WHERE shard_key = ?1",
            [shard_key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    let Some((writer, binding, placement, frontier)) = publication else {
        return Ok(false);
    };
    Ok(
        decode::<RemoteWriterAuthorityV1>(&writer)? == authority.writer
            && decode::<StoreRuntimeBindingV1>(&binding)? == authority.binding
            && decode_u64(placement)? == authority.placement_revision
            && Some(decode::<ShardWatermarkV1>(&frontier)?) == authority.frontier,
    )
}

fn capture_storage(error: RemoteAuthorityStorageErrorV1) -> RemoteCapturePersistenceErrorV1 {
    match error {
        RemoteAuthorityStorageErrorV1::Corruption
        | RemoteAuthorityStorageErrorV1::Encoding
        | RemoteAuthorityStorageErrorV1::InvalidContract => {
            RemoteCapturePersistenceErrorV1::Corruption
        }
        _ => RemoteCapturePersistenceErrorV1::Unavailable,
    }
}

fn same_remote_target(left: &RemoteWriterAuthorityV1, right: &RemoteWriterAuthorityV1) -> bool {
    left.project_id == right.project_id
        && left.scope == right.scope
        && left.authority.fence.brain_id == right.authority.fence.brain_id
        && left.authority.fence.shard_id == right.authority.fence.shard_id
        && left.authority.fence.generation_id == right.authority.fence.generation_id
}

fn validate_frontier(
    binding: &StoreRuntimeBindingV1,
    frontier: &ShardWatermarkV1,
) -> Result<(), RemoteAuthorityStorageErrorV1> {
    if frontier.shard_id != binding.shard_id
        || frontier.incarnation != binding.incarnation
        || frontier.authority_epoch != binding.authority_epoch
        || frontier.commit_sequence.0 == 0
    {
        return Err(RemoteAuthorityStorageErrorV1::InvalidContract);
    }
    Ok(())
}

fn validate_sink_id(sink_id: &str) -> Result<(), RemoteAuthorityStorageErrorV1> {
    if sink_id.is_empty()
        || sink_id.len() > 128
        || sink_id.trim() != sink_id
        || sink_id.chars().any(char::is_control)
    {
        return Err(RemoteAuthorityStorageErrorV1::InvalidContract);
    }
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> Result<String, RemoteAuthorityStorageErrorV1> {
    serde_json::to_string(value).map_err(|_| RemoteAuthorityStorageErrorV1::Encoding)
}

fn decode<T: DeserializeOwned>(value: &str) -> Result<T, RemoteAuthorityStorageErrorV1> {
    serde_json::from_str(value).map_err(|_| RemoteAuthorityStorageErrorV1::Corruption)
}

fn sqlite_u64(value: u64) -> Result<i64, RemoteAuthorityStorageErrorV1> {
    i64::try_from(value).map_err(|_| RemoteAuthorityStorageErrorV1::InvalidContract)
}

fn decode_u64(value: i64) -> Result<u64, RemoteAuthorityStorageErrorV1> {
    u64::try_from(value).map_err(|_| RemoteAuthorityStorageErrorV1::Corruption)
}

fn storage(error: rusqlite::Error) -> RemoteAuthorityStorageErrorV1 {
    RemoteAuthorityStorageErrorV1::Storage(error.to_string())
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteAuthorityStorageErrorV1 {
    #[error("remote authority contract is invalid")]
    InvalidContract,
    #[error("remote authority is already initialized")]
    AlreadyInitialized,
    #[error("remote authority compare-and-swap conflicted")]
    CasConflict,
    #[error("remote authority is unavailable")]
    AuthorityUnavailable,
    #[error("remote durable sink fence is stale")]
    StaleFence,
    #[error("old remote authority is still writable")]
    OldAuthorityStillWritable,
    #[error("remote durable sink fence is missing: {sink_id}")]
    MissingFence { sink_id: String },
    #[error("remote authority record is corrupt")]
    Corruption,
    #[error("remote authority encoding failed")]
    Encoding,
    #[error("remote authority storage is unavailable")]
    Unavailable,
    #[error("remote authority storage failed: {0}")]
    Storage(String),
}

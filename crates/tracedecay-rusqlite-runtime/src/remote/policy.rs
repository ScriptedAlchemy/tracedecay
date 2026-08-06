use tracedecay_application::ResolvedScope;
use tracedecay_application::remote::query::{
    RemoteExactObservationQueryErrorV1, RemoteQueryAuthorizationEvidenceV1,
    RemoteQueryAuthorizationPortV1, RemoteQueryPolicyRecordV1,
};
use tracedecay_application::remote::replay::{
    RemoteReplayApplicationErrorV1, RemoteReplayFrameV1, RemoteReplayPolicyDecisionV1,
    RemoteReplayPolicyEvidencePortV1, RemoteReplayPolicyEvidenceV1, RemoteReplayPolicyPortV1,
};
use tracedecay_domain::{
    CanonicalObservationIdV1, RemoteRepositoryScopeV1, RemoteWriterFenceV1, UtcMicros,
    canonical_sha256,
};

use super::*;

impl RemoteSqliteStorageV1 {
    pub fn recovery_policy_digest(
        &self,
        scope: &RemoteRepositoryScopeV1,
    ) -> Result<tracedecay_domain::ManifestDigest, RemoteReplayApplicationErrorV1> {
        self.load_replay_policy(scope)
            .map(|evidence| evidence.policy.digest)
    }

    pub fn store_replay_policy(
        &self,
        evidence: &RemoteReplayPolicyEvidenceV1,
    ) -> Result<(), RemoteReplayApplicationErrorV1> {
        evidence.validate()?;
        let scope_digest = replay_scope_digest(&evidence.repository_scope)?;
        let encoded = serde_json::to_string(evidence)
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        self.handle
            .execute(
                statement(
                    "INSERT INTO remote_replay_policies (
                        scope_digest, policy_revision, evidence_json
                     ) VALUES (?1, ?2, ?3)
                     ON CONFLICT(scope_digest) DO UPDATE SET
                        policy_revision = excluded.policy_revision,
                        evidence_json = excluded.evidence_json
                     WHERE excluded.policy_revision > remote_replay_policies.policy_revision
                        OR (
                            excluded.policy_revision = remote_replay_policies.policy_revision
                            AND excluded.evidence_json = remote_replay_policies.evidence_json
                        )",
                    vec![
                        text(scope_digest.as_str()),
                        ExactSqlValue::Integer(
                            i64::try_from(evidence.policy_revision)
                                .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?,
                        ),
                        text(&encoded),
                    ],
                )
                .map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?,
            )
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?;
        if self.load_replay_policy(&evidence.repository_scope)? != *evidence {
            return Err(RemoteReplayApplicationErrorV1::PolicyMismatch);
        }
        Ok(())
    }

    pub fn store_query_policy(
        &self,
        record: &RemoteQueryPolicyRecordV1,
    ) -> Result<(), RemoteExactObservationQueryErrorV1> {
        record.validate()?;
        let scope_digest = query_scope_digest(&record.repository_scope)?;
        let encoded = serde_json::to_string(record)
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        self.handle
            .execute(
                statement(
                    "INSERT INTO remote_query_policies (
                        scope_digest, policy_revision, record_json
                     ) VALUES (?1, ?2, ?3)
                     ON CONFLICT(scope_digest) DO UPDATE SET
                        policy_revision = excluded.policy_revision,
                        record_json = excluded.record_json
                     WHERE excluded.policy_revision > remote_query_policies.policy_revision
                        OR (
                            excluded.policy_revision = remote_query_policies.policy_revision
                            AND excluded.record_json = remote_query_policies.record_json
                        )",
                    vec![
                        text(scope_digest.as_str()),
                        ExactSqlValue::Integer(
                            i64::try_from(record.policy_revision).map_err(|_| {
                                RemoteExactObservationQueryErrorV1::PolicyUnavailable
                            })?,
                        ),
                        text(&encoded),
                    ],
                )
                .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?,
            )
            .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        if self.load_query_policy(&record.repository_scope)? != *record {
            return Err(RemoteExactObservationQueryErrorV1::PolicyUnavailable);
        }
        Ok(())
    }

    fn load_replay_policy(
        &self,
        scope: &RemoteRepositoryScopeV1,
    ) -> Result<RemoteReplayPolicyEvidenceV1, RemoteReplayApplicationErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT evidence_json FROM remote_replay_policies WHERE scope_digest = ?1",
            vec![text(replay_scope_digest(scope)?.as_str())],
        )
        .map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?;
        let row = one_row(rows).map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?;
        let evidence = serde_json::from_str(
            row_text(&row, 0).map_err(|_| RemoteReplayApplicationErrorV1::PolicyUnavailable)?,
        )
        .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        Ok(evidence)
    }

    fn load_query_policy(
        &self,
        scope: &RemoteRepositoryScopeV1,
    ) -> Result<RemoteQueryPolicyRecordV1, RemoteExactObservationQueryErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT record_json FROM remote_query_policies WHERE scope_digest = ?1",
            vec![text(query_scope_digest(scope)?.as_str())],
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        let row =
            one_row(rows).map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        let record = serde_json::from_str(
            row_text(&row, 0).map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?,
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)?;
        Ok(record)
    }
}

impl RemoteReplayPolicyPortV1 for RemoteSqliteStorageV1 {
    fn authorize_current_policy(
        &self,
        frame: &RemoteReplayFrameV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteReplayPolicyDecisionV1, RemoteReplayApplicationErrorV1> {
        let evidence = self.load_replay_policy(&frame.capture.writer.scope)?;
        evidence.validate_for(frame)?;
        if evidence.revalidated_at > observed_at {
            return Err(RemoteReplayApplicationErrorV1::PolicyMismatch);
        }
        Ok(evidence.decision)
    }
}

impl RemoteReplayPolicyEvidencePortV1 for RemoteSqliteStorageV1 {
    fn current_policy_evidence(
        &self,
        frame: &RemoteReplayFrameV1,
    ) -> Result<RemoteReplayPolicyEvidenceV1, RemoteReplayApplicationErrorV1> {
        let evidence = self.load_replay_policy(&frame.capture.writer.scope)?;
        evidence.validate_for(frame)?;
        Ok(evidence)
    }
}

impl RemoteQueryAuthorizationPortV1 for RemoteSqliteStorageV1 {
    fn authorize(
        &self,
        scope: &ResolvedScope,
        repository_scope: &RemoteRepositoryScopeV1,
        observation_id: &CanonicalObservationIdV1,
        expected_authority: &RemoteWriterFenceV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteQueryAuthorizationEvidenceV1, RemoteExactObservationQueryErrorV1> {
        let record = self.load_query_policy(repository_scope)?;
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

fn replay_scope_digest(
    scope: &RemoteRepositoryScopeV1,
) -> Result<ManifestDigest, RemoteReplayApplicationErrorV1> {
    canonical_sha256(&("tracedecay.remote-replay-policy-scope.v2", scope))
        .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)
}

fn query_scope_digest(
    scope: &RemoteRepositoryScopeV1,
) -> Result<ManifestDigest, RemoteExactObservationQueryErrorV1> {
    canonical_sha256(&("tracedecay.remote-query-policy-scope.v2", scope))
        .map_err(|_| RemoteExactObservationQueryErrorV1::PolicyUnavailable)
}

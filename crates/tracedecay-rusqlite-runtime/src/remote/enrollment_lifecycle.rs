use tracedecay_contracts::remote::auth::revoke_credential;
use tracedecay_domain::{CredentialRevocationReceiptV1, EnrollmentCredentialRecordV1};

use super::*;

impl RemoteSqliteStorageV1 {
    /// Revokes every enrollment `node_id` holds in this store and returns one
    /// receipt per enrollment. A row revoked earlier keeps its original
    /// revocation and receipt and is not rewritten, so a repeat, or a rerun
    /// after a crash between rows, converges on the same state.
    pub fn revoke_node_enrollments(
        &self,
        node_id: &BrainNodeId,
        revoked_at: UtcMicros,
    ) -> Result<Vec<CredentialRevocationReceiptV1>, RemoteSqliteStorageErrorV1> {
        let rows = query(
            self.handle(),
            "SELECT enrollment_json FROM remote_enrollments
             WHERE node_id = ?1 ORDER BY enrollment_id",
            vec![text(node_id.as_str())],
        )?;
        rows.rows
            .iter()
            .map(|row| {
                let current: EnrollmentCredentialRecordV1 = serde_json::from_str(
                    crate::exact_sql::text_at(&row.values, 0)
                        .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
                )
                .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
                let at = current
                    .revoked_at
                    .unwrap_or_else(|| revoked_at.max(current.issued_at));
                let (next, receipt) = revoke_credential(&current, current.revision, at)
                    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
                if current.revoked_at.is_none() {
                    self.revoke_enrollment(&current, &next, &receipt)?;
                }
                Ok(receipt)
            })
            .collect()
    }

    /// Spends every grant issued to `node_id` that no enrollment has
    /// consumed yet and returns the ids this call spent. A spent grant is
    /// refused at credential lookup like any consumed grant, and
    /// reprovisioning it is refused as a conflict. A repeat finds nothing
    /// left to spend.
    ///
    /// Revocation reuses `consumed_at`: the grant record and its admission
    /// evidence are digest-bound at issuance, so neither can carry a later
    /// revocation, and the store contract has no column for one. A spent
    /// grant without an enrollment row is therefore a revoked grant.
    pub fn revoke_node_grants(
        &self,
        node_id: &BrainNodeId,
        revoked_at: UtcMicros,
    ) -> Result<Vec<EntityId>, RemoteSqliteStorageErrorV1> {
        let rows = query(
            self.handle(),
            "SELECT grant_json FROM remote_enrollment_grants
             WHERE consumed_at IS NULL ORDER BY grant_id",
            Vec::new(),
        )?;
        let mut revoked = Vec::new();
        for row in &rows.rows {
            let grant: EnrollmentGrantV1 = serde_json::from_str(
                crate::exact_sql::text_at(&row.values, 0)
                    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
            )
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
            if grant.node_id != *node_id {
                continue;
            }
            // Zero rows means a concurrent enrollment consumed the grant
            // first; the node's enrollment revocation covers that row.
            let spent = self.handle().execute(ExactSqlStatement::new(
                "UPDATE remote_enrollment_grants SET consumed_at = ?1
                 WHERE grant_id = ?2 AND consumed_at IS NULL"
                    .to_owned(),
                vec![
                    ExactSqlValue::Integer(revoked_at.0),
                    text(grant.grant_id.as_str()),
                ],
            )?)?;
            if spent.changed_rows == 1 {
                revoked.push(grant.grant_id);
            }
        }
        Ok(revoked)
    }

    pub fn revoke_enrollment(
        &self,
        expected: &EnrollmentCredentialRecordV1,
        replacement: &EnrollmentCredentialRecordV1,
        receipt: &CredentialRevocationReceiptV1,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        expected
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        replacement
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let idempotent = expected == replacement
            && expected.revoked_at == Some(receipt.revoked_at)
            && receipt.current_revision == expected.revision
            && receipt.prior_revision == expected.revision.saturating_sub(1);
        let newly_revoked = replacement.revision == expected.revision.checked_add(1).unwrap_or(0)
            && expected.revoked_at.is_none()
            && replacement.revoked_at == Some(receipt.revoked_at)
            && receipt.prior_revision == expected.revision
            && receipt.current_revision == replacement.revision;
        if receipt.enrollment_id != expected.enrollment_id
            || receipt.node_id != expected.node_id
            || replacement.enrollment_id != expected.enrollment_id
            || replacement.brain_id != expected.brain_id
            || replacement.node_id != expected.node_id
            || replacement.fingerprint != expected.fingerprint
            || replacement.issued_at != expected.issued_at
            || replacement.expires_at != expected.expires_at
            || replacement.scope != expected.scope
            || replacement.capabilities != expected.capabilities
            || (!idempotent && !newly_revoked)
        {
            return Err(RemoteSqliteStorageErrorV1::Corruption);
        }
        replace_enrollment(self, expected, replacement)
    }
}

fn replace_enrollment(
    storage: &RemoteSqliteStorageV1,
    expected: &EnrollmentCredentialRecordV1,
    replacement: &EnrollmentCredentialRecordV1,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    let expected_json =
        serde_json::to_string(expected).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    let replacement_json =
        serde_json::to_string(replacement).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
    let result = storage.handle().execute(ExactSqlStatement::new(
        "UPDATE remote_enrollments
         SET revision = ?1, credential_fingerprint = ?2, enrollment_json = ?3
         WHERE enrollment_id = ?4 AND revision = ?5 AND enrollment_json = ?6"
            .to_owned(),
        vec![
            ExactSqlValue::Integer(
                i64::try_from(replacement.revision)
                    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
            ),
            text(replacement.fingerprint.digest().as_str()),
            text(&replacement_json),
            text(expected.enrollment_id.as_str()),
            ExactSqlValue::Integer(
                i64::try_from(expected.revision)
                    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
            ),
            text(&expected_json),
        ],
    )?)?;
    if result.changed_rows != 1 {
        return Err(RemoteSqliteStorageErrorV1::Conflict);
    }
    Ok(())
}

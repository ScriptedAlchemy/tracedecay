//! Process-local configuration grant authority for trusted shipped adapters.
//!
//! The daemon remains the authority for remote invocations. Local CLI and
//! loopback dashboard adapters retain this exact-project authority so they can
//! use the same policy recheck and transactional store path without minting an
//! unauthenticated transport grant.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use tracedecay_domain::configuration::{
    ConfigurationGrantId, ConfigurationGrantReceiptId, ConfigurationMutationEffectV1,
    ConfigurationMutationGrantReceiptV1, ConfigurationMutationOperationV1,
    ConfigurationMutationSinkV1, ConfigurationRevisionId,
};
use tracedecay_domain::{AccessPolicyDigest, ActorId, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_policy::configuration::{
    ConfigurationMutationGrantSnapshotV1, ConfigurationMutationGrantStateV1,
    ConfigurationMutationPermissionV1,
};

use super::authorization::{
    ConfigurationMutationGrantAuthority, ConfigurationMutationGrantAuthorityError,
    ConfigurationMutationGrantAuthorityFuture,
};
use super::types::{ConfigurationError, ConfigurationMutationAuthority};

#[derive(Clone)]
pub(crate) struct EphemeralConfigurationGrantAuthority {
    actor: ActorId,
    policy_epoch: u64,
    policy_digest: AccessPolicyDigest,
    expires_at: UtcMicros,
    grants: Arc<RwLock<BTreeMap<ConfigurationGrantId, ConfigurationMutationGrantSnapshotV1>>>,
}

impl EphemeralConfigurationGrantAuthority {
    pub(crate) fn new(
        actor: ActorId,
        policy_digest: AccessPolicyDigest,
        expires_at: UtcMicros,
    ) -> Self {
        Self {
            actor,
            policy_epoch: 1,
            policy_digest,
            expires_at,
            grants: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn issue(
        &self,
        nonce: u64,
        operation: ConfigurationMutationOperationV1,
        scope_digest: ManifestDigest,
        expected_revision: ConfigurationRevisionId,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
        issued_at: UtcMicros,
    ) -> Result<ConfigurationMutationAuthority, ConfigurationError> {
        let expires_at = UtcMicros(
            issued_at
                .0
                .saturating_add(30_000_000)
                .min(self.expires_at.0),
        );
        if issued_at >= expires_at {
            return Err(ConfigurationError::MutationAuthorityRejected);
        }
        let grant_id =
            ConfigurationGrantId::new(format!("configuration.grant.local-runtime-{nonce}"))
                .map_err(ConfigurationError::validation)?;
        let receipt_id = ConfigurationGrantReceiptId::new(format!(
            "configuration.grant-receipt.local-runtime-{nonce}"
        ))
        .map_err(ConfigurationError::validation)?;
        let permission = ConfigurationMutationPermissionV1 {
            operation,
            sink,
            effect,
        };
        let grant_digest = canonical_sha256(&(
            "tracedecay.local-runtime.configuration-grant.v1",
            &grant_id,
            &self.actor,
            &scope_digest,
            &expected_revision,
            permission,
            self.policy_epoch,
            &self.policy_digest,
            issued_at,
            expires_at,
        ))
        .map_err(ConfigurationError::validation)?;
        let receipt = ConfigurationMutationGrantReceiptV1::issue(
            receipt_id,
            grant_id.clone(),
            self.actor.clone(),
            operation,
            scope_digest.clone(),
            expected_revision.clone(),
            self.policy_epoch,
            self.policy_digest.clone(),
            sink,
            effect,
            issued_at,
            expires_at,
        )
        .map_err(ConfigurationError::validation)?;
        let snapshot = ConfigurationMutationGrantSnapshotV1 {
            grant_id: grant_id.clone(),
            grant_revision: 1,
            grant_digest,
            authorized_receipt_digest: receipt.receipt_digest.clone(),
            actor_id: self.actor.clone(),
            scope_digest,
            expected_configuration_revision: expected_revision,
            permissions: BTreeSet::from([permission]),
            policy_epoch: self.policy_epoch,
            policy_digest: self.policy_digest.clone(),
            issued_at,
            expires_at,
            state: ConfigurationMutationGrantStateV1::Active,
        };
        if !snapshot.is_valid() {
            return Err(ConfigurationError::Unavailable);
        }
        self.grants
            .write()
            .map_err(|_| ConfigurationError::Unavailable)?
            .insert(grant_id, snapshot);
        Ok(ConfigurationMutationAuthority { receipt })
    }
}

impl ConfigurationMutationGrantAuthority for EphemeralConfigurationGrantAuthority {
    fn current_grant<'a>(
        &'a self,
        grant_id: &'a ConfigurationGrantId,
    ) -> ConfigurationMutationGrantAuthorityFuture<'a> {
        let result = self
            .grants
            .read()
            .map_err(|_| ConfigurationMutationGrantAuthorityError::Unavailable)
            .and_then(|grants| {
                grants
                    .get(grant_id)
                    .cloned()
                    .ok_or(ConfigurationMutationGrantAuthorityError::Rejected)
            });
        Box::pin(async move { result })
    }
}

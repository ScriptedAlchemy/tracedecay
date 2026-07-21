//! Policy-backed configuration mutation authorization for internal callers.
//!
//! Transport binding remains outside this module. Callers supply only a grant
//! receipt; the adapter reloads its current immutable grant authority and runs
//! the approved pure policy evaluator immediately before the effect.

use tracedecay_domain::UtcMicros;
use tracedecay_domain::configuration::{
    ConfigurationGrantId, ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationRevisionId,
};
use tracedecay_policy::configuration::{
    ConfigurationMutationGrantSnapshotV1, ConfigurationMutationPolicyEvaluator,
    ConfigurationMutationPolicyEvaluatorV1, ConfigurationMutationRecheckDispositionV1,
    ConfigurationMutationRecheckInputV1,
};

use super::ports::{
    ConfigurationMutationAuthorizationPort, CurrentConfigurationMutationAuthorizationV1,
};
use super::types::ConfigurationError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationMutationGrantAuthorityError {
    Rejected,
    Unavailable,
}

/// Current grant authority lookup. Missing, denied, expired, and revoked
/// grants must all return `Rejected`; adapters must not expose which occurred.
pub trait ConfigurationMutationGrantAuthority {
    fn current_grant(
        &self,
        grant_id: &ConfigurationGrantId,
    ) -> Result<ConfigurationMutationGrantSnapshotV1, ConfigurationMutationGrantAuthorityError>;
}

pub struct PolicyBackedConfigurationMutationAuthorization<Authority, Evaluator> {
    authority: Authority,
    evaluator: Evaluator,
}

impl<Authority>
    PolicyBackedConfigurationMutationAuthorization<
        Authority,
        ConfigurationMutationPolicyEvaluatorV1,
    >
{
    pub fn new(authority: Authority) -> Self {
        Self {
            authority,
            evaluator: ConfigurationMutationPolicyEvaluatorV1,
        }
    }
}

impl<Authority, Evaluator> PolicyBackedConfigurationMutationAuthorization<Authority, Evaluator> {
    pub fn with_evaluator(authority: Authority, evaluator: Evaluator) -> Self {
        Self {
            authority,
            evaluator,
        }
    }
}

impl<Authority, Evaluator> ConfigurationMutationAuthorizationPort
    for PolicyBackedConfigurationMutationAuthorization<Authority, Evaluator>
where
    Authority: ConfigurationMutationGrantAuthority + Sync,
    Evaluator: ConfigurationMutationPolicyEvaluator + Sync,
{
    fn recheck(
        &self,
        receipt: &ConfigurationMutationGrantReceiptV1,
        operation: ConfigurationMutationOperationV1,
        expected_revision: &ConfigurationRevisionId,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
        now: UtcMicros,
    ) -> Result<CurrentConfigurationMutationAuthorizationV1, ConfigurationError> {
        receipt
            .validate()
            .map_err(|_| ConfigurationError::MutationAuthorityRejected)?;
        let current =
            self.authority
                .current_grant(&receipt.grant_id)
                .map_err(|error| match error {
                    ConfigurationMutationGrantAuthorityError::Rejected => {
                        ConfigurationError::MutationAuthorityRejected
                    }
                    ConfigurationMutationGrantAuthorityError::Unavailable => {
                        ConfigurationError::Unavailable
                    }
                })?;
        let disposition = self.evaluator.evaluate(
            &current,
            ConfigurationMutationRecheckInputV1 {
                receipt,
                operation,
                expected_revision,
                sink,
                effect,
                evaluated_at: now,
            },
        );
        if disposition != ConfigurationMutationRecheckDispositionV1::Allow {
            return Err(ConfigurationError::MutationAuthorityRejected);
        }
        Ok(CurrentConfigurationMutationAuthorizationV1 {
            scope_digest: current.scope_digest,
            policy_epoch: current.policy_epoch,
            policy_digest: current.policy_digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use tracedecay_domain::configuration::{
        ConfigurationGrantReceiptId, ConfigurationMutationGrantReceiptV1,
    };
    use tracedecay_domain::{AccessPolicyDigest, ActorId, ManifestDigest};
    use tracedecay_policy::configuration::{
        ConfigurationMutationGrantStateV1, ConfigurationMutationPermissionV1,
    };

    #[derive(Clone)]
    struct Authority(
        Result<ConfigurationMutationGrantSnapshotV1, ConfigurationMutationGrantAuthorityError>,
    );

    impl ConfigurationMutationGrantAuthority for Authority {
        fn current_grant(
            &self,
            _grant_id: &ConfigurationGrantId,
        ) -> Result<ConfigurationMutationGrantSnapshotV1, ConfigurationMutationGrantAuthorityError>
        {
            self.0.clone()
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn policy_digest(byte: char) -> AccessPolicyDigest {
        AccessPolicyDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn fixture() -> (
        ConfigurationMutationGrantSnapshotV1,
        ConfigurationMutationGrantReceiptV1,
    ) {
        let operation = ConfigurationMutationOperationV1::DirectMutation;
        let sink = ConfigurationMutationSinkV1::ConfigurationStore;
        let effect = ConfigurationMutationEffectV1::CommitConfigurationRevision;
        let revision = id::<ConfigurationRevisionId>("configuration.revision.fixture");
        let snapshot = ConfigurationMutationGrantSnapshotV1 {
            grant_id: id("configuration.grant.fixture"),
            grant_revision: 1,
            grant_digest: digest('a'),
            actor_id: id::<ActorId>("actor.fixture"),
            scope_digest: digest('b'),
            expected_configuration_revision: revision.clone(),
            permissions: BTreeSet::from([ConfigurationMutationPermissionV1 {
                operation,
                sink,
                effect,
            }]),
            policy_epoch: 7,
            policy_digest: policy_digest('c'),
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(20),
            state: ConfigurationMutationGrantStateV1::Active,
        };
        let receipt = ConfigurationMutationGrantReceiptV1::issue(
            id::<ConfigurationGrantReceiptId>("configuration.grant-receipt.fixture"),
            snapshot.grant_id.clone(),
            snapshot.actor_id.clone(),
            operation,
            snapshot.scope_digest.clone(),
            revision,
            snapshot.policy_epoch,
            snapshot.policy_digest.clone(),
            sink,
            effect,
            snapshot.issued_at,
            snapshot.expires_at,
        )
        .unwrap();
        (snapshot, receipt)
    }

    #[test]
    fn current_policy_grant_authorizes_exact_internal_effect() {
        let (snapshot, receipt) = fixture();
        let service =
            PolicyBackedConfigurationMutationAuthorization::new(Authority(Ok(snapshot.clone())));
        let current = service
            .recheck(
                &receipt,
                receipt.operation,
                &receipt.expected_configuration_revision,
                receipt.sink,
                receipt.effect,
                UtcMicros(19),
            )
            .unwrap();
        assert_eq!(current.scope_digest, snapshot.scope_digest);
        assert_eq!(current.policy_epoch, snapshot.policy_epoch);
        assert_eq!(current.policy_digest, snapshot.policy_digest);
    }

    #[test]
    fn hidden_and_revoked_grants_share_one_rejection() {
        let (mut snapshot, receipt) = fixture();
        let hidden = PolicyBackedConfigurationMutationAuthorization::new(Authority(Err(
            ConfigurationMutationGrantAuthorityError::Rejected,
        )));
        let hidden_error = hidden
            .recheck(
                &receipt,
                receipt.operation,
                &receipt.expected_configuration_revision,
                receipt.sink,
                receipt.effect,
                UtcMicros(19),
            )
            .unwrap_err();

        snapshot.state = ConfigurationMutationGrantStateV1::Revoked;
        let revoked = PolicyBackedConfigurationMutationAuthorization::new(Authority(Ok(snapshot)));
        let revoked_error = revoked
            .recheck(
                &receipt,
                receipt.operation,
                &receipt.expected_configuration_revision,
                receipt.sink,
                receipt.effect,
                UtcMicros(19),
            )
            .unwrap_err();

        assert_eq!(hidden_error, ConfigurationError::MutationAuthorityRejected);
        assert_eq!(revoked_error, hidden_error);
    }
}

//! Pure configuration-mutation grant rechecks.
//!
//! Policy consumes an immutable current grant snapshot supplied by the
//! application authority. It cannot load, issue, renew, or widen grants and it
//! never performs a configuration effect.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{
    ConfigurationGrantId, ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationRevisionId,
};
use tracedecay_domain::{AccessPolicyDigest, ActorId, ManifestDigest, UtcMicros};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationMutationGrantStateV1 {
    Active,
    Revoked,
    Stale,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationMutationPermissionV1 {
    pub operation: ConfigurationMutationOperationV1,
    pub sink: ConfigurationMutationSinkV1,
    pub effect: ConfigurationMutationEffectV1,
}

/// Immutable current authority for one configuration grant.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationMutationGrantSnapshotV1 {
    pub grant_id: ConfigurationGrantId,
    pub grant_revision: u64,
    pub grant_digest: ManifestDigest,
    pub authorized_receipt_digest: ManifestDigest,
    pub actor_id: ActorId,
    pub scope_digest: ManifestDigest,
    pub expected_configuration_revision: ConfigurationRevisionId,
    pub permissions: BTreeSet<ConfigurationMutationPermissionV1>,
    pub policy_epoch: u64,
    pub policy_digest: AccessPolicyDigest,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub state: ConfigurationMutationGrantStateV1,
}

impl ConfigurationMutationGrantSnapshotV1 {
    pub fn is_valid(&self) -> bool {
        self.grant_id.validate().is_ok()
            && self.grant_revision > 0
            && self.grant_digest.validate().is_ok()
            && self.authorized_receipt_digest.validate().is_ok()
            && self.actor_id.validate().is_ok()
            && self.scope_digest.validate().is_ok()
            && self.expected_configuration_revision.validate().is_ok()
            && !self.permissions.is_empty()
            && self.policy_epoch > 0
            && self.policy_digest.validate().is_ok()
            && self.issued_at < self.expires_at
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ConfigurationMutationRecheckInputV1<'a> {
    pub receipt: &'a ConfigurationMutationGrantReceiptV1,
    pub operation: ConfigurationMutationOperationV1,
    pub expected_revision: &'a ConfigurationRevisionId,
    pub sink: ConfigurationMutationSinkV1,
    pub effect: ConfigurationMutationEffectV1,
    pub evaluated_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationMutationRecheckDispositionV1 {
    Allow,
    Deny,
    Indeterminate,
}

pub trait ConfigurationMutationPolicyEvaluator {
    fn evaluate(
        &self,
        current: &ConfigurationMutationGrantSnapshotV1,
        input: ConfigurationMutationRecheckInputV1<'_>,
    ) -> ConfigurationMutationRecheckDispositionV1;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ConfigurationMutationPolicyEvaluatorV1;

impl ConfigurationMutationPolicyEvaluator for ConfigurationMutationPolicyEvaluatorV1 {
    fn evaluate(
        &self,
        current: &ConfigurationMutationGrantSnapshotV1,
        input: ConfigurationMutationRecheckInputV1<'_>,
    ) -> ConfigurationMutationRecheckDispositionV1 {
        if !current.is_valid() {
            return ConfigurationMutationRecheckDispositionV1::Indeterminate;
        }
        match current.state {
            ConfigurationMutationGrantStateV1::Revoked => {
                return ConfigurationMutationRecheckDispositionV1::Deny;
            }
            ConfigurationMutationGrantStateV1::Stale
            | ConfigurationMutationGrantStateV1::Ambiguous => {
                return ConfigurationMutationRecheckDispositionV1::Indeterminate;
            }
            ConfigurationMutationGrantStateV1::Active => {}
        }
        if input.evaluated_at < current.issued_at
            || input.evaluated_at >= current.expires_at
            || input.receipt.grant_id != current.grant_id
            || input.receipt.receipt_digest != current.authorized_receipt_digest
            || input.receipt.issued_at != current.issued_at
            || input.receipt.expires_at != current.expires_at
            || input.receipt.policy_epoch != current.policy_epoch
            || input.receipt.policy_digest != current.policy_digest
            || input.expected_revision != &current.expected_configuration_revision
            || !current
                .permissions
                .contains(&ConfigurationMutationPermissionV1 {
                    operation: input.operation,
                    sink: input.sink,
                    effect: input.effect,
                })
        {
            return ConfigurationMutationRecheckDispositionV1::Deny;
        }
        if input
            .receipt
            .validate_for(
                &current.actor_id,
                input.operation,
                &current.scope_digest,
                input.expected_revision,
                input.sink,
                input.effect,
                input.evaluated_at,
            )
            .is_err()
        {
            return ConfigurationMutationRecheckDispositionV1::Deny;
        }
        ConfigurationMutationRecheckDispositionV1::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::configuration::ConfigurationGrantReceiptId;

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
        let mut snapshot = ConfigurationMutationGrantSnapshotV1 {
            grant_id: id("configuration.grant.fixture"),
            grant_revision: 1,
            grant_digest: digest('a'),
            authorized_receipt_digest: digest('d'),
            actor_id: id("actor.fixture"),
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
            Some(
                tracedecay_domain::configuration::ConfigurationIdempotencyKey::new(
                    "configuration.idempotency.policy-fixture",
                )
                .unwrap(),
            ),
            snapshot.issued_at,
            snapshot.expires_at,
        )
        .unwrap();
        snapshot.authorized_receipt_digest = receipt.receipt_digest.clone();
        (snapshot, receipt)
    }

    #[test]
    fn current_exact_grant_allows_only_its_bound_effect() {
        let (snapshot, receipt) = fixture();
        let disposition = ConfigurationMutationPolicyEvaluatorV1.evaluate(
            &snapshot,
            ConfigurationMutationRecheckInputV1 {
                receipt: &receipt,
                operation: receipt.operation,
                expected_revision: &receipt.expected_configuration_revision,
                sink: receipt.sink,
                effect: receipt.effect,
                evaluated_at: UtcMicros(19),
            },
        );
        assert_eq!(
            disposition,
            ConfigurationMutationRecheckDispositionV1::Allow
        );
    }

    #[test]
    fn revoked_and_expanded_grants_never_allow() {
        let (mut snapshot, receipt) = fixture();
        snapshot.state = ConfigurationMutationGrantStateV1::Revoked;
        let input = ConfigurationMutationRecheckInputV1 {
            receipt: &receipt,
            operation: receipt.operation,
            expected_revision: &receipt.expected_configuration_revision,
            sink: receipt.sink,
            effect: receipt.effect,
            evaluated_at: UtcMicros(19),
        };
        assert_eq!(
            ConfigurationMutationPolicyEvaluatorV1.evaluate(&snapshot, input),
            ConfigurationMutationRecheckDispositionV1::Deny
        );

        snapshot.state = ConfigurationMutationGrantStateV1::Active;
        assert_eq!(
            ConfigurationMutationPolicyEvaluatorV1.evaluate(
                &snapshot,
                ConfigurationMutationRecheckInputV1 {
                    operation: ConfigurationMutationOperationV1::CredentialWrite,
                    ..input
                },
            ),
            ConfigurationMutationRecheckDispositionV1::Deny
        );

        snapshot.authorized_receipt_digest = digest('e');
        assert_eq!(
            ConfigurationMutationPolicyEvaluatorV1.evaluate(&snapshot, input),
            ConfigurationMutationRecheckDispositionV1::Deny
        );
        snapshot.authorized_receipt_digest = receipt.receipt_digest.clone();
        snapshot.grant_revision += 1;
        snapshot.grant_digest = digest('d');
        snapshot.issued_at = UtcMicros(11);
        assert_eq!(
            ConfigurationMutationPolicyEvaluatorV1.evaluate(&snapshot, input),
            ConfigurationMutationRecheckDispositionV1::Deny
        );
    }

    #[test]
    fn direct_grant_recheck_denies_a_swapped_idempotency_key() {
        let (snapshot, mut receipt) = fixture();
        receipt.idempotency_key = Some(
            tracedecay_domain::configuration::ConfigurationIdempotencyKey::new(
                "configuration.idempotency.swapped",
            )
            .unwrap(),
        );
        let disposition = ConfigurationMutationPolicyEvaluatorV1.evaluate(
            &snapshot,
            ConfigurationMutationRecheckInputV1 {
                operation: receipt.operation,
                expected_revision: &receipt.expected_configuration_revision,
                sink: receipt.sink,
                effect: receipt.effect,
                receipt: &receipt,
                evaluated_at: UtcMicros(19),
            },
        );

        assert_eq!(disposition, ConfigurationMutationRecheckDispositionV1::Deny);
    }
}

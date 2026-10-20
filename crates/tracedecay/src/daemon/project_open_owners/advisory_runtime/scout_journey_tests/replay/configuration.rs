use tracedecay_agent_hosts::agents::context_scout::address_registry::ContextScoutConfigurationPinV1;
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationValueV1,
    TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_domain::{AccessPolicyDigest, UtcMicros};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::configuration::GlobalDbConfigurationControlStore;
use tracedecay_global_db::configuration::contracts::{
    ConfigurationControlStore, ConfigurationMutationAuthority, DirectConfigurationMutation,
};
use tracedecay_global_db::configuration::store::commit_direct_in_transaction;

use super::super::{now_utc_micros, typed_id};

pub(super) async fn advance_configuration(
    sessions: &RegisteredGlobalDb,
) -> ContextScoutConfigurationPinV1 {
    let store = GlobalDbConfigurationControlStore::new_registered(sessions);
    let current = store.current().await.unwrap();
    let key = typed_id(TELEMETRY_TIMINGS_SETTING_KEY);
    let ConfigurationValueV1::Boolean(previous) = current.snapshot.effective_values[&key] else {
        panic!("canonical timings configuration must be boolean");
    };
    let mutation = DirectConfigurationMutation::Set {
        layer: ConfigurationLayerIdV1::Project {
            project_id: sessions
                .binding()
                .shard_id
                .scope
                .project_id()
                .unwrap()
                .clone(),
        },
        key,
        value: Box::new(ConfigurationValueV1::Boolean(!previous)),
    };
    let now = now_utc_micros();
    let authority = ConfigurationMutationAuthority {
        receipt: ConfigurationMutationGrantReceiptV1::issue(
            typed_id("configuration.grant-receipt.scout-replay"),
            typed_id("configuration.grant.scout-replay"),
            typed_id("actor.scout-replay"),
            ConfigurationMutationOperationV1::DirectMutation,
            mutation.target_scope_digest().unwrap(),
            current.revision_id.clone(),
            1,
            AccessPolicyDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            ConfigurationMutationSinkV1::ConfigurationStore,
            ConfigurationMutationEffectV1::CommitConfigurationRevision,
            Some(typed_id("configuration.idempotency.scout-replay")),
            now,
            UtcMicros(now.0 + 10_000_000),
        )
        .unwrap(),
    };
    let transaction = sessions.begin_write_transaction().await.unwrap();
    let committed =
        commit_direct_in_transaction(&transaction, &authority, &mutation, &current.revision_id)
            .await
            .unwrap();
    transaction.commit().await.unwrap();
    assert_ne!(committed.current.revision_id, current.revision_id);
    assert_ne!(
        committed.current.snapshot.effective_behavior_digest,
        current.snapshot.effective_behavior_digest
    );
    ContextScoutConfigurationPinV1::from_current(&committed.current).unwrap()
}

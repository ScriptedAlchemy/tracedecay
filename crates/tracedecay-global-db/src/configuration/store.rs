//! `SQLite` persistence behind the final configuration control plane.

use super::contracts::{
    ActivationDriftV1, AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT,
    ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationControlStore, ConfigurationCurrentStateV1, ConfigurationError,
    ConfigurationMutationAuthority, ConfigurationMutationReceipt, ConfigurationOperationFuture,
    ConfigurationRollbackRequest, ConfigurationSettlementAuthorityV1, DirectConfigurationMutation,
    ScopeRevalidationEvidenceV1,
};
use super::registry::ConfigurationRegistry;
use super::resolver::{ConfigurationResolutionV1, registry_default_candidate};
use super::schema::ConfigurationSchemaError;
use crate::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use thiserror::Error;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CandidateDispositionV1, ChangePlanId,
    CodeIndexWorkerSelectionV1, ConfigurationAuditEvent, ConfigurationAuditEventId,
    ConfigurationAuditEventKindV1, ConfigurationCandidateV1, ConfigurationIdempotencyKey,
    ConfigurationLayerIdV1, ConfigurationReceiptId, ConfigurationRevisionId,
    ConfigurationSnapshotV1, ConfigurationValueV1, INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY,
    LCM_SUMMARIZER_EXECUTABLES_SETTING_KEY, ProtectedChange, ProtectedChangePlan,
    ProtectedChangeSnapshotError, RETIRED_CORE_SETTING_KEYS_V1, RedactedConfigurationChangeV1,
    RollbackModeV1, RuleEffect, SOURCE_BINDINGS_SETTING_KEY,
    SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY, ScopeControlOperationV1, SettingKey, SourceKindV1,
    USER_CODE_INDEX_WORKERS_SETTING_KEY, USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY,
    USER_UPLOAD_ENABLED_SETTING_KEY, USER_WATCHER_DEBOUNCE_MS_SETTING_KEY, UserProfileId,
    WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{AccessPolicyDigest, ActorId, ManifestDigest, UtcMicros, canonical_sha256};
#[cfg(test)]
use tracedecay_runtime_core::db::engine::TestConnection;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_store::StoreShardScopeV1;
use tracedecay_store::configuration::{
    ConfigurationCommitV1, ConfigurationMutationReceiptV1, ConfigurationProtectedOperationV1,
    ConfigurationProtectedPlanRecordV1, ConfigurationRevisionRecordV1, ConfigurationRevisionStore,
    ConfigurationStoreError, ConfigurationStoreResult,
};

mod activation;
mod audit;
mod codec;
mod control;
mod mutation;
mod read;
mod revision;
mod write;

use activation::{
    StoredComponentActivationState, advance_component_desired_state,
    insert_component_activation_event, latest_component_activation_state,
    validate_activation_error_code, validate_component_name,
};
use codec::{StoredConfigurationProtectedOperationV1, invalid_store_data, unavailable_store};
use mutation::{
    ConfigurationCommitDraft, commit_direct_in_transaction_with_registry,
    current_state_from_transaction, derived_identifier, map_store_error,
};
use read::read_revision_from_executor;
use read::{
    validate_snapshot_registry_completeness, validate_snapshot_registry_completeness_with_registry,
};
use revision::{insert_revision, insert_revision_with_registry};

pub use mutation::{
    ConfigurationDirectCommitOutcomeV1, commit_direct_in_transaction, protected_change_snapshot_v1,
};

#[derive(Debug, Error)]
pub enum ConfigurationStorageError {
    #[error("configuration schema error: {0}")]
    Schema(#[from] ConfigurationSchemaError),
    #[error("configuration storage error: {0}")]
    Sql(#[from] tracedecay_runtime_core::db::engine::Error),
    #[error("configuration storage encoded invalid data: {0}")]
    Encoding(String),
}

/// Concrete control-plane adapter over one already-open owned session store.
/// It never accepts an arbitrary connection, opens a fallback database, or
/// owns policy resolution; every write obtains the selected store's serialized
/// immediate transaction and commits all durable effects together.
pub struct GlobalDbConfigurationControlStore<'db> {
    db: &'db RegisteredGlobalDb,
}

impl<'db> GlobalDbConfigurationControlStore<'db> {
    #[hotpath::skip]
    pub const fn new_registered(db: &'db RegisteredGlobalDb) -> Self {
        Self { db }
    }

    /// Reports whether this exact final-shape control-plane store has no
    /// revision yet.
    pub fn is_uninitialized(&self) -> ConfigurationOperationFuture<'_, bool> {
        Box::pin(async move {
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let mut table_rows = read
                .query(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table'
                       AND name = 'configuration_revisions'",
                    (),
                )
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let table_count = table_rows
                .next()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?
                .ok_or_else(|| {
                    ConfigurationError::validation_message(
                        "configuration table presence query returned no row",
                    )
                })?
                .get::<i64>(0)
                .map_err(|_| {
                    ConfigurationError::validation_message(
                        "configuration table count is not an integer",
                    )
                })?;
            if table_count != 1 {
                return Err(ConfigurationError::validation_message(
                    "configuration final-shape table is unavailable",
                ));
            }
            let mut rows = read
                .query("SELECT COUNT(*) FROM configuration_revisions", ())
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let row = rows
                .next()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?
                .ok_or_else(|| {
                    ConfigurationError::validation_message(
                        "configuration initialization query returned no row",
                    )
                })?;
            let revision_count = row.get::<i64>(0).map_err(|_| {
                ConfigurationError::validation_message(
                    "configuration revision count is not an integer",
                )
            })?;
            if rows
                .next()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?
                .is_some()
            {
                return Err(ConfigurationError::validation_message(
                    "configuration initialization query returned multiple rows",
                ));
            }
            Ok(revision_count == 0)
        })
    }

    /// Publishes the sole canonical first revision into an empty final-shape
    /// store. No legacy input, path, environment, or fallback value is read.
    #[hotpath::skip]
    pub async fn initialize_canonical(
        &self,
        revision_id: &ConfigurationRevisionId,
        resolution: &ConfigurationResolutionV1,
        occurred_at: UtcMicros,
    ) -> Result<(), ConfigurationError> {
        let registry = ConfigurationRegistry::core().map_err(ConfigurationError::validation)?;
        self.initialize_canonical_with_registry(revision_id, resolution, occurred_at, &registry)
            .await
    }

    #[hotpath::measure(future = true, label = "global_db.configuration.persist.init")]
    async fn initialize_canonical_with_registry(
        &self,
        revision_id: &ConfigurationRevisionId,
        resolution: &ConfigurationResolutionV1,
        occurred_at: UtcMicros,
        registry: &ConfigurationRegistry,
    ) -> Result<(), ConfigurationError> {
        revision_id
            .validate()
            .map_err(ConfigurationError::validation)?;
        resolution
            .snapshot
            .validate()
            .map_err(ConfigurationError::validation)?;
        validate_snapshot_registry_completeness_with_registry(&resolution.snapshot, registry)
            .map_err(map_store_error)?;
        let transaction = self
            .db
            .begin_write_transaction()
            .await
            .map_err(|_| ConfigurationError::Unavailable)?;
        let outcome = async {
            let mut rows = transaction
                .query("SELECT COUNT(*) FROM configuration_revisions", ())
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let existing = rows
                .next()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?
                .ok_or_else(|| {
                    ConfigurationError::validation_message(
                        "configuration initialization count returned no row",
                    )
                })?
                .get::<i64>(0)
                .map_err(|_| {
                    ConfigurationError::validation_message(
                        "configuration initialization count is not an integer",
                    )
                })?;
            if existing != 0 {
                return Err(ConfigurationError::RevisionConflict);
            }
            let actor_id = ActorId::new("actor.configuration-initialization".to_owned())
                .map_err(ConfigurationError::validation)?;
            let revision = ConfigurationRevisionRecordV1 {
                revision_id: revision_id.clone(),
                parent_revision_id: None,
                snapshot: resolution.snapshot.clone(),
                actor_id,
                operation_kind: "canonical_initialization".to_owned(),
                created_at: occurred_at,
            };
            insert_revision_with_registry(&transaction, &revision, registry)
                .await
                .map_err(map_store_error)
        }
        .await;
        match outcome {
            Ok(()) => transaction
                .commit()
                .await
                .map_err(|_| ConfigurationError::Unavailable),
            Err(error) => Err(error),
        }
    }

    /// Publishes one daemon-owned source binding: the project-open binding
    /// rebound to a moved checkout's locator digest, or the GitHub binding
    /// derived from the checkout's `origin` remote.
    ///
    /// This is a daemon-owned identity-preserving write, not an operator
    /// scope change: the caller derives `binding` from identity it already
    /// holds (the registered project and its checkout). Only `BindSource` and
    /// `RebindSource` are accepted, and both pass the same validator as a
    /// protected apply. The write is a compare-and-swap against the revision
    /// the caller read; concurrent mutation surfaces as a typed
    /// `RevisionConflict` and the caller re-reads.
    #[hotpath::measure(future = true, label = "global_db.configuration.persist.rebind")]
    pub async fn publish_daemon_source_binding(
        &self,
        expected_revision_id: &ConfigurationRevisionId,
        change: &ProtectedChange,
        occurred_at: UtcMicros,
    ) -> Result<ConfigurationCurrentStateV1, ConfigurationError> {
        expected_revision_id
            .validate()
            .map_err(ConfigurationError::validation)?;
        let operation_kind = match change {
            ProtectedChange::BindSource(_) => "daemon_source_binding_bind",
            ProtectedChange::RebindSource(_) => "daemon_source_binding_rebind",
            _ => {
                return Err(ConfigurationError::validation_message(
                    "a daemon-owned source binding write must bind or rebind a source",
                ));
            }
        };
        change.validate().map_err(ConfigurationError::validation)?;
        let transaction = self
            .db
            .begin_write_transaction()
            .await
            .map_err(|_| ConfigurationError::Unavailable)?;
        let outcome = async {
            let current = current_state_from_transaction(&transaction).await?;
            if &current.revision_id != expected_revision_id {
                return Err(ConfigurationError::RevisionConflict);
            }
            let operation_digest =
                canonical_sha256(&("tracedecay.configuration.daemon-source-binding.v1", change))
                    .map_err(ConfigurationError::validation)?;
            let next_revision_id: ConfigurationRevisionId = derived_identifier(
                "configuration.revision.v1",
                &canonical_sha256(&(
                    "tracedecay.configuration.result-revision.v1",
                    expected_revision_id,
                    &operation_digest,
                ))
                .map_err(ConfigurationError::validation)?,
                "configuration daemon source binding revision id",
            )?;
            let snapshot =
                protected_change_snapshot_v1(&current.snapshot, change, &next_revision_id)?;
            validate_snapshot_registry_completeness(&snapshot).map_err(map_store_error)?;
            let actor_id = ActorId::new("actor.tracedecay-daemon.source-binding".to_owned())
                .map_err(ConfigurationError::validation)?;
            let revision = ConfigurationRevisionRecordV1 {
                revision_id: next_revision_id.clone(),
                parent_revision_id: Some(expected_revision_id.clone()),
                snapshot,
                actor_id,
                operation_kind: operation_kind.to_owned(),
                created_at: occurred_at,
            };
            insert_revision(&transaction, &revision)
                .await
                .map_err(map_store_error)?;
            advance_component_desired_state(&transaction, &next_revision_id, occurred_at)
                .await
                .map_err(map_store_error)?;
            Ok(ConfigurationCurrentStateV1 {
                revision_id: revision.revision_id,
                snapshot: revision.snapshot,
            })
        }
        .await;
        match outcome {
            Ok(state) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| ConfigurationError::Unavailable)?;
                Ok(state)
            }
            Err(error) => Err(error),
        }
    }

    /// Converge a snapshot written by an earlier published registry onto the
    /// current one: registry-added settings receive their typed defaults and
    /// retired settings are dropped.
    ///
    /// This is exact schema convergence, not a runtime fallback: only the
    /// closed sets of known additive keys and known retired keys are accepted.
    /// The result is an immutable child revision, and the expected parent is
    /// checked under the store's write transaction.
    pub async fn converge_registered_registry_shape(
        &self,
        expected_revision_id: &ConfigurationRevisionId,
        occurred_at: UtcMicros,
    ) -> Result<ConfigurationCurrentStateV1, ConfigurationError> {
        let registry = ConfigurationRegistry::core().map_err(ConfigurationError::validation)?;
        self.converge_registry_shape(
            expected_revision_id,
            occurred_at,
            &registry,
            &[
                INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY,
                LCM_SUMMARIZER_EXECUTABLES_SETTING_KEY,
                SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY,
            ],
            RETIRED_CORE_SETTING_KEYS_V1,
        )
        .await
    }

    #[hotpath::measure(future = true, label = "global_db.configuration.persist.converge")]
    async fn converge_registry_shape(
        &self,
        expected_revision_id: &ConfigurationRevisionId,
        occurred_at: UtcMicros,
        registry: &ConfigurationRegistry,
        additive_keys: &[&str],
        retired_keys: &[&str],
    ) -> Result<ConfigurationCurrentStateV1, ConfigurationError> {
        expected_revision_id
            .validate()
            .map_err(ConfigurationError::validation)?;
        let transaction = self
            .db
            .begin_write_transaction()
            .await
            .map_err(|_| ConfigurationError::Unavailable)?;
        let outcome = async {
            let current = current_state_from_transaction(&transaction).await?;
            if &current.revision_id != expected_revision_id {
                return Err(ConfigurationError::RevisionConflict);
            }
            let additive_keys = additive_keys
                .iter()
                .copied()
                .map(SettingKey::new)
                .collect::<Result<std::collections::BTreeSet<_>, _>>()
                .map_err(ConfigurationError::validation)?;
            let retired_keys = retired_keys
                .iter()
                .copied()
                .map(SettingKey::new)
                .collect::<Result<std::collections::BTreeSet<_>, _>>()
                .map_err(ConfigurationError::validation)?;
            let expected_keys = registry
                .definitions()
                .map(|definition| definition.key.clone())
                .collect::<std::collections::BTreeSet<_>>();
            let actual_keys = current
                .snapshot
                .effective_values
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            let missing_keys = expected_keys
                .difference(&actual_keys)
                .cloned()
                .collect::<Vec<_>>();
            let removals = actual_keys
                .intersection(&retired_keys)
                .cloned()
                .collect::<Vec<_>>();
            if missing_keys.is_empty() && removals.is_empty() {
                validate_snapshot_registry_completeness_with_registry(&current.snapshot, registry)
                    .map_err(map_store_error)?;
                return Ok(current);
            }
            let surviving_keys = actual_keys
                .difference(&retired_keys)
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            if missing_keys.iter().any(|key| !additive_keys.contains(key))
                || !surviving_keys.is_subset(&expected_keys)
            {
                return Err(ConfigurationError::ResetRequired {
                    reason: "configuration snapshot registry drift is not a registered additive-default or retired-key upgrade"
                        .to_owned(),
                });
            }
            let additions = missing_keys
                .iter()
                .map(|key| {
                    registry
                        .definition(key)
                        .map(|definition| (key.clone(), definition.default_value.clone()))
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(ConfigurationError::validation)?;
            let operation_digest = canonical_sha256(&(
                "tracedecay.configuration.registry-shape-convergence.v1",
                expected_revision_id,
                &additions,
                &removals,
            ))
            .map_err(ConfigurationError::validation)?;
            let next_revision_id: ConfigurationRevisionId = derived_identifier(
                "configuration.revision.v1",
                &canonical_sha256(&(
                    "tracedecay.configuration.result-revision.v1",
                    expected_revision_id,
                    &operation_digest,
                ))
                .map_err(ConfigurationError::validation)?,
                "configuration registry shape revision id",
            )?;
            let mut effective_values = current.snapshot.effective_values.clone();
            let mut provenance = current.snapshot.provenance.clone();
            for key in &removals {
                effective_values.remove(key);
                provenance.remove(key);
            }
            for (key, default_value) in additions {
                effective_values.insert(key.clone(), default_value);
                provenance.insert(
                    key,
                    vec![registry_default_candidate().map_err(ConfigurationError::validation)?],
                );
            }
            let snapshot = ConfigurationSnapshotV1::new(effective_values, provenance)
                .map_err(ConfigurationError::validation)?;
            validate_snapshot_registry_completeness_with_registry(&snapshot, registry)
                .map_err(map_store_error)?;
            let revision = ConfigurationRevisionRecordV1 {
                revision_id: next_revision_id.clone(),
                parent_revision_id: Some(expected_revision_id.clone()),
                snapshot,
                actor_id: ActorId::new(
                    "actor.tracedecay-daemon.registry-shape-convergence".to_owned(),
                )
                .map_err(ConfigurationError::validation)?,
                operation_kind: "registry_shape_convergence".to_owned(),
                created_at: occurred_at,
            };
            insert_revision_with_registry(&transaction, &revision, registry)
                .await
                .map_err(map_store_error)?;
            advance_component_desired_state(&transaction, &next_revision_id, occurred_at)
                .await
                .map_err(map_store_error)?;
            Ok(ConfigurationCurrentStateV1 {
                revision_id: revision.revision_id,
                snapshot: revision.snapshot,
            })
        }
        .await;
        match outcome {
            Ok(state) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| ConfigurationError::Unavailable)?;
                Ok(state)
            }
            Err(error) => Err(error),
        }
    }

    /// Records a daemon/component activation result. Failed activation keeps
    /// the prior last-working observed revision while advancing desired state.
    pub fn record_component_activation(
        &self,
        component: String,
        observed_revision_id: Option<ConfigurationRevisionId>,
        activation_error_code: Option<String>,
        occurred_at: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ()> {
        Box::pin(async move {
            validate_component_name(&component).map_err(map_store_error)?;
            validate_activation_error_code(activation_error_code.as_deref())
                .map_err(map_store_error)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                let current = current_state_from_transaction(&transaction).await?;
                if let Some(observed_revision_id) = &observed_revision_id
                    && read_revision_from_executor(&transaction, observed_revision_id)
                        .await
                        .map_err(map_store_error)?
                        .is_none()
                {
                    return Err(ConfigurationError::PlanStale);
                }
                let prior = latest_component_activation_state(&transaction, &component)
                    .await
                    .map_err(map_store_error)?;
                let prior_last_working = prior
                    .as_ref()
                    .and_then(|state| state.last_working_revision_id.clone())
                    .or_else(|| {
                        prior
                            .as_ref()
                            .and_then(|state| state.observed_revision_id.clone())
                    });
                let failed = activation_error_code.is_some();
                let last_working_revision_id = if failed {
                    prior_last_working.clone()
                } else {
                    observed_revision_id
                        .clone()
                        .or_else(|| prior_last_working.clone())
                };
                let observed_revision_id = if failed {
                    prior_last_working
                } else {
                    observed_revision_id
                };
                let restart_required =
                    observed_revision_id.as_ref() != Some(&current.revision_id) || failed;
                insert_component_activation_event(
                    &transaction,
                    &StoredComponentActivationState {
                        component,
                        desired_revision_id: current.revision_id,
                        observed_revision_id,
                        last_working_revision_id,
                        restart_required,
                        activation_error_code,
                    },
                    occurred_at,
                )
                .await
                .map_err(map_store_error)
            }
            .await;
            match outcome {
                Ok(()) => transaction
                    .commit()
                    .await
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }
}

/// The profile's resolved configuration: one snapshot of every
/// [`ConfigurationRegistry::profile`] setting in the registered
/// `ProfileSessions` store. Its revision is independent from every project
/// configuration revision and is the only valid CAS token for profile
/// setting changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileConfigurationV1 {
    pub revision_id: ConfigurationRevisionId,
    pub snapshot: ConfigurationSnapshotV1,
}

impl ProfileConfigurationV1 {
    pub fn value(&self, key: &str) -> Result<&ConfigurationValueV1, ConfigurationError> {
        let key = SettingKey::new(key).map_err(ConfigurationError::validation)?;
        self.snapshot.effective_values.get(&key).ok_or_else(|| {
            ConfigurationError::validation_message(format!(
                "profile configuration is missing {}",
                key.as_str()
            ))
        })
    }

    pub fn code_index_workers(
        &self,
    ) -> Result<ProfileCodeIndexWorkerConfigurationV1, ConfigurationError> {
        let ConfigurationValueV1::CodeIndexWorkerSelection(selection) =
            self.value(USER_CODE_INDEX_WORKERS_SETTING_KEY)?
        else {
            return Err(ConfigurationError::validation_message(
                "profile code-index worker configuration has the wrong value kind",
            ));
        };
        selection
            .validate()
            .map_err(ConfigurationError::validation)?;
        Ok(ProfileCodeIndexWorkerConfigurationV1 {
            revision_id: self.revision_id.clone(),
            snapshot_id: self.snapshot.snapshot_id.clone(),
            selection: *selection,
        })
    }
}

/// The daemon-wide code-index worker selection within the profile
/// configuration, pinned to the profile revision it was read at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileCodeIndexWorkerConfigurationV1 {
    pub revision_id: ConfigurationRevisionId,
    pub snapshot_id: tracedecay_domain::configuration::ConfigurationSnapshotId,
    pub selection: CodeIndexWorkerSelectionV1,
}

#[derive(Clone, Debug)]
pub struct ProfileConfigurationCommitV1 {
    pub receipt: ConfigurationMutationReceipt,
    pub current: ProfileConfigurationV1,
}

#[derive(Clone, Debug)]
pub struct ProfileCodeIndexWorkerCommitV1 {
    pub receipt: ConfigurationMutationReceipt,
    pub current: ProfileCodeIndexWorkerConfigurationV1,
}

/// Profile settings registered after a profile store was first initialized.
/// A store written before one existed converges by adding its default.
const PROFILE_ADDITIVE_SETTING_KEYS: &[&str] = &[
    USER_UPLOAD_ENABLED_SETTING_KEY,
    USER_WATCHER_DEBOUNCE_MS_SETTING_KEY,
    USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY,
];

/// Exact adapter over one registered `ProfileSessions` database.
pub struct ProfileConfigurationStore<'db> {
    store: GlobalDbConfigurationControlStore<'db>,
    profile_id: UserProfileId,
}

impl<'db> ProfileConfigurationStore<'db> {
    pub fn new_registered(
        db: &'db RegisteredGlobalDb,
        profile_id: &UserProfileId,
    ) -> Result<Self, ConfigurationError> {
        let binding = db.binding();
        if binding.shard_id.profile_id != *profile_id
            || binding.shard_id.scope != StoreShardScopeV1::ProfileSessions
        {
            return Err(ConfigurationError::validation_message(
                "profile configuration requires the exact registered profile-sessions database",
            ));
        }
        Ok(Self {
            store: GlobalDbConfigurationControlStore::new_registered(db),
            profile_id: profile_id.clone(),
        })
    }

    /// The current profile configuration, initializing a fresh store from the
    /// registry defaults and converging one written by an earlier registry.
    #[hotpath::skip]
    pub async fn read_or_initialize(
        &self,
        occurred_at: UtcMicros,
    ) -> Result<ProfileConfigurationV1, ConfigurationError> {
        let registry = ConfigurationRegistry::profile().map_err(ConfigurationError::validation)?;
        if self.store.is_uninitialized().await? {
            let initial_revision_id =
                ConfigurationRevisionId::new("configuration.profile.initial.v1".to_owned())
                    .map_err(ConfigurationError::validation)?;
            let resolution = super::resolver::resolve_configuration(&registry, &[])
                .map_err(ConfigurationError::validation)?;
            match self
                .store
                .initialize_canonical_with_registry(
                    &initial_revision_id,
                    &resolution,
                    occurred_at,
                    &registry,
                )
                .await
            {
                Ok(()) | Err(ConfigurationError::RevisionConflict) => {}
                Err(error) => return Err(error),
            }
        }
        let current = self.current_state().await?;
        if registry.definitions().all(|definition| {
            current
                .snapshot
                .effective_values
                .contains_key(&definition.key)
        }) {
            return self.project(current, &registry);
        }
        let converged = match self
            .store
            .converge_registry_shape(
                &current.revision_id,
                occurred_at,
                &registry,
                PROFILE_ADDITIVE_SETTING_KEYS,
                &[],
            )
            .await
        {
            Ok(converged) => converged,
            Err(ConfigurationError::RevisionConflict) => self.current_state().await?,
            Err(error) => return Err(error),
        };
        self.project(converged, &registry)
    }

    #[hotpath::measure(future = true, label = "global_db.configuration.query")]
    async fn current_state(&self) -> Result<ConfigurationCurrentStateV1, ConfigurationError> {
        let read = self
            .store
            .db
            .read_snapshot()
            .await
            .map_err(|_| ConfigurationError::Unavailable)?;
        current_state_from_transaction(&read).await
    }

    pub fn code_index_worker_mutation(
        &self,
        selection: CodeIndexWorkerSelectionV1,
    ) -> Result<DirectConfigurationMutation, ConfigurationError> {
        selection
            .validate()
            .map_err(ConfigurationError::validation)?;
        Ok(DirectConfigurationMutation::Set {
            layer: ConfigurationLayerIdV1::UserProfile {
                profile_id: self.profile_id.clone(),
            },
            key: SettingKey::new(USER_CODE_INDEX_WORKERS_SETTING_KEY)
                .map_err(ConfigurationError::validation)?,
            value: Box::new(ConfigurationValueV1::CodeIndexWorkerSelection(selection)),
        })
    }

    #[hotpath::measure(future = true, label = "global_db.configuration.persist.profile")]
    pub async fn commit_direct(
        &self,
        authority: &ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> Result<ProfileConfigurationCommitV1, ConfigurationError> {
        let registry = ConfigurationRegistry::profile().map_err(ConfigurationError::validation)?;
        let transaction = self
            .store
            .db
            .begin_write_transaction()
            .await
            .map_err(|_| ConfigurationError::Unavailable)?;
        let outcome = commit_direct_in_transaction_with_registry(
            &transaction,
            authority,
            mutation,
            expected_revision,
            &registry,
        )
        .await;
        match outcome {
            Ok(outcome) => {
                let current = self.project(outcome.current, &registry)?;
                transaction
                    .commit()
                    .await
                    .map_err(|_| ConfigurationError::Unavailable)?;
                Ok(ProfileConfigurationCommitV1 {
                    receipt: outcome.receipt,
                    current,
                })
            }
            Err(error) => Err(error),
        }
    }

    #[hotpath::measure(future = true, label = "global_db.configuration.persist.selection")]
    pub async fn commit_selection(
        &self,
        authority: &ConfigurationMutationAuthority,
        selection: CodeIndexWorkerSelectionV1,
        expected_revision: &ConfigurationRevisionId,
    ) -> Result<ProfileCodeIndexWorkerCommitV1, ConfigurationError> {
        let mutation = self.code_index_worker_mutation(selection)?;
        let committed = self
            .commit_direct(authority, &mutation, expected_revision)
            .await?;
        Ok(ProfileCodeIndexWorkerCommitV1 {
            receipt: committed.receipt,
            current: committed.current.code_index_workers()?,
        })
    }

    /// Every value is registered and typed, and every winning candidate is
    /// this profile's own layer or the registry default.
    fn project(
        &self,
        current: ConfigurationCurrentStateV1,
        registry: &ConfigurationRegistry,
    ) -> Result<ProfileConfigurationV1, ConfigurationError> {
        validate_snapshot_registry_completeness_with_registry(&current.snapshot, registry)
            .map_err(map_store_error)?;
        for key in current.snapshot.effective_values.keys() {
            let provenance = current.snapshot.provenance.get(key).ok_or_else(|| {
                ConfigurationError::validation_message(format!(
                    "profile configuration is missing provenance for {}",
                    key.as_str()
                ))
            })?;
            if provenance.iter().any(|candidate| match &candidate.layer {
                ConfigurationLayerIdV1::Default => false,
                ConfigurationLayerIdV1::UserProfile { profile_id } => {
                    profile_id != &self.profile_id
                }
                ConfigurationLayerIdV1::Project { .. }
                | ConfigurationLayerIdV1::Collection { .. } => true,
            }) {
                return Err(ConfigurationError::validation_message(format!(
                    "profile configuration provenance for {} does not match the registered profile",
                    key.as_str()
                )));
            }
        }
        let current = ProfileConfigurationV1 {
            revision_id: current.revision_id,
            snapshot: current.snapshot,
        };
        current.code_index_workers()?;
        Ok(current)
    }
}

/// Cloneable configuration authority for daemon-owned runtimes.
///
/// The adapter retains the daemon's already-open project runtime database and
/// creates a scoped borrowed adapter for each operation. It therefore reuses
/// the canonical transaction, revision, and compare-and-swap implementation
/// without opening another database or resolving configuration independently.
/// It does not extend the owning daemon's database-authority lease: writes fail
/// closed after that owner exits.
#[derive(Clone)]
pub struct OwnedGlobalDbConfigurationControlStore {
    /// The exact daemon-registered project-runtime database handle. Production
    /// composition always supplies it at construction; no later attachment,
    /// path reopen, or authority substitution is available.
    db: RegisteredGlobalDbLeaseV1,
}

impl OwnedGlobalDbConfigurationControlStore {
    pub fn from_registered_project_runtime_db(db: RegisteredGlobalDbLeaseV1) -> Self {
        Self { db }
    }

    fn database(&self) -> RegisteredGlobalDbLeaseV1 {
        self.db.clone()
    }

    /// Revalidate mutation access by acquiring the exact guarded writer
    /// transaction. The capability carries the client-bound authority, so no
    /// path-derived authority can be substituted here.
    #[hotpath::skip]
    async fn require_active_mutation_scope(
        db: &RegisteredGlobalDb,
    ) -> Result<(), ConfigurationError> {
        let transaction = db
            .begin_write_transaction()
            .await
            .map_err(|_| ConfigurationError::Unavailable)?;
        transaction
            .rollback()
            .await
            .map_err(|_| ConfigurationError::Unavailable)
    }

    pub fn record_component_activation(
        &self,
        component: String,
        observed_revision_id: Option<ConfigurationRevisionId>,
        activation_error_code: Option<String>,
        occurred_at: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ()> {
        let db = self.database();
        Box::pin(async move {
            Self::require_active_mutation_scope(db.as_ref()).await?;
            let store = GlobalDbConfigurationControlStore::new_registered(db.as_ref());
            store
                .record_component_activation(
                    component,
                    observed_revision_id,
                    activation_error_code,
                    occurred_at,
                )
                .await
        })
    }

    /// Reads the exact revision last observed by one retained component.
    /// The revision is resolved through the canonical configuration history;
    /// callers never retain a parallel activation baseline.
    pub fn observed_component_configuration(
        &self,
        component: String,
    ) -> ConfigurationOperationFuture<'_, Option<ConfigurationCurrentStateV1>> {
        let db = self.database();
        Box::pin(async move {
            validate_component_name(&component).map_err(map_store_error)?;
            let read = db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let state = latest_component_activation_state(&read, &component)
                .await
                .map_err(map_store_error)?;
            let Some(revision_id) = state.and_then(|state| {
                state
                    .observed_revision_id
                    .or(state.last_working_revision_id)
            }) else {
                return Ok(None);
            };
            let revision = read_revision_from_executor(&read, &revision_id)
                .await
                .map_err(map_store_error)?
                .ok_or_else(|| {
                    ConfigurationError::validation_message(
                        "observed component configuration revision is unavailable",
                    )
                })?;
            Ok(Some(ConfigurationCurrentStateV1 {
                revision_id: revision.revision_id,
                snapshot: revision.snapshot,
            }))
        })
    }
}

/// Forwards one owned-store method to a freshly registered borrowed store.
///
/// Every method below is the same shape: clone the borrowed arguments so the
/// returned future owns them, retain the database handle, then open the
/// registered store inside the future and delegate. Only the argument list and
/// the delegated call differ, so they are all the macro takes.
macro_rules! forward_to_registered {
    ($self:ident, [$($owned:ident),* $(,)?], mutating, |$store:ident| $call:expr) => {{
        let db = $self.database();
        $(let $owned = $owned.clone();)*
        Box::pin(async move {
            OwnedGlobalDbConfigurationControlStore::require_active_mutation_scope(db.as_ref())
                .await?;
            let $store = GlobalDbConfigurationControlStore::new_registered(db.as_ref());
            $call.await
        })
    }};
    ($self:ident, [$($owned:ident),* $(,)?], |$store:ident| $call:expr) => {{
        let db = $self.database();
        $(let $owned = $owned.clone();)*
        Box::pin(async move {
            let $store = GlobalDbConfigurationControlStore::new_registered(db.as_ref());
            $call.await
        })
    }};
}

impl ConfigurationControlStore for OwnedGlobalDbConfigurationControlStore {
    fn current(&self) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
        forward_to_registered!(self, [], |store| store.current())
    }

    fn save_plan(
        &self,
        plan: &ProtectedChangePlan,
        operation: &ProtectedChange,
    ) -> ConfigurationOperationFuture<'_, ()> {
        forward_to_registered!(self, [plan, operation], mutating, |store| store
            .save_plan(&plan, &operation))
    }

    fn load_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationOperationFuture<'_, Option<ProtectedChangePlan>> {
        forward_to_registered!(self, [plan_id], |store| store.load_plan(&plan_id))
    }

    fn replay_apply(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        operation: tracedecay_domain::configuration::ConfigurationMutationOperationV1,
    ) -> ConfigurationOperationFuture<'_, Option<ConfigurationMutationReceipt>> {
        forward_to_registered!(self, [authority, request], |store| store
            .replay_apply(&authority, &request, operation))
    }

    fn commit_direct(
        &self,
        authority: &ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        forward_to_registered!(
            self,
            [authority, mutation, expected_revision],
            mutating,
            |store| { store.commit_direct(&authority, &mutation, &expected_revision) }
        )
    }

    fn commit_protected(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        forward_to_registered!(
            self,
            [authority, request, plan, evidence],
            mutating,
            |store| store.commit_protected(&authority, &request, &plan, &evidence)
        )
    }

    fn dry_run_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        rollback: &ConfigurationRollbackRequest,
        now: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        forward_to_registered!(self, [authority, rollback], mutating, |store| store
            .dry_run_rollback(&authority, &rollback, now))
    }

    fn apply_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        forward_to_registered!(
            self,
            [authority, request, plan, evidence],
            mutating,
            |store| store.apply_rollback(&authority, &request, &plan, &evidence)
        )
    }

    fn audit(
        &self,
        actor: &AuthorizedActor,
        query: &ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
        // Disambiguated: the registered store also has an inherent `audit`.
        forward_to_registered!(self, [actor, query], |store| {
            ConfigurationControlStore::audit(&store, &actor, &query)
        })
    }

    fn observed_state(
        &self,
        actor: &AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
        forward_to_registered!(self, [actor], |store| store.observed_state(&actor))
    }
}

#[cfg(test)]
mod tests;

//! `SQLite` persistence behind the final configuration control plane.

use std::sync::Arc;

use super::contracts::{
    ActivationDriftV1, AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT,
    ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationControlStore, ConfigurationCurrentStateV1, ConfigurationError,
    ConfigurationMutationAuthority, ConfigurationMutationReceipt, ConfigurationOperationFuture,
    ConfigurationRollbackRequest, ConfigurationSettlementAuthorityV1, CredentialWritePort,
    DirectConfigurationMutation, ScopeRevalidationEvidenceV1, WriteOnlyCredentialMutation,
};
use super::registry::ConfigurationRegistry;
use super::resolver::{ConfigurationResolutionV1, registry_default_candidate};
use super::schema::ConfigurationSchemaError;
#[cfg(test)]
use super::schema::ensure_configuration_schema;
use crate::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use thiserror::Error;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CandidateDispositionV1, ChangePlanId,
    ConfigurationAuditEvent, ConfigurationAuditEventId, ConfigurationAuditEventKindV1,
    ConfigurationCandidateV1, ConfigurationIdempotencyKey, ConfigurationLayerIdV1,
    ConfigurationReceiptId, ConfigurationRevisionId, ConfigurationSnapshotV1, ConfigurationValueV1,
    CredentialKindV1, CredentialReferenceId, CredentialReferenceMetadataV1, ProtectedChange,
    ProtectedChangePlan, ProtectedChangeSnapshotError, RedactedConfigurationChangeV1,
    RollbackModeV1, RuleEffect, SOURCE_BINDINGS_SETTING_KEY, ScopeControlOperationV1,
    ScopeSourceBinding, SettingKey, SourceKindV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{AccessPolicyDigest, ActorId, ManifestDigest, UtcMicros, canonical_sha256};
#[cfg(test)]
use tracedecay_runtime_core::db::engine::{Connection, TestConnection, TransactionBehavior};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_store::configuration::{
    ConfigurationCommitV1, ConfigurationMutationReceiptV1, ConfigurationProtectedOperationV1,
    ConfigurationProtectedPlanRecordV1, ConfigurationRevisionRecordV1, ConfigurationRevisionStore,
    ConfigurationStoreError, ConfigurationStoreResult,
};

mod activation;
mod audit;
mod codec;
mod control;
mod credential;
mod mutation;
mod read;
mod revision;
mod write;

use activation::{
    StoredComponentActivationState, advance_component_desired_state,
    insert_component_activation_event, latest_component_activation_state,
    validate_activation_error_code, validate_component_name,
};
#[cfg(test)]
use audit::decode_audit_row;
use codec::{StoredConfigurationProtectedOperationV1, invalid_store_data, unavailable_store};
#[cfg(test)]
use mutation::commit_configuration_transaction;
#[cfg(test)]
use mutation::validate_commit_bindings;
use mutation::{
    ConfigurationCommitDraft, current_state_from_transaction, derived_identifier,
    map_protected_change_snapshot_error, map_store_error,
};
use read::read_revision_from_executor;
use read::validate_snapshot_registry_completeness;
#[cfg(test)]
use read::{current_revision_id_from_executor, read_change_plan_from_executor};
use revision::insert_revision;
#[cfg(test)]
use write::insert_change_plan;

pub use mutation::{ConfigurationDirectCommitOutcomeV1, commit_direct_in_transaction};

#[derive(Debug, Error)]
pub enum ConfigurationStorageError {
    #[error("configuration schema error: {0}")]
    Schema(#[from] ConfigurationSchemaError),
    #[error("configuration storage error: {0}")]
    Sql(#[from] tracedecay_runtime_core::db::engine::Error),
    #[error("configuration storage encoded invalid data: {0}")]
    Encoding(String),
}

/// Connection-local SQL helper used only behind the registered database's writer and
/// read-snapshot lanes. It is deliberately not a public authority surface.
#[cfg(test)]
struct ConfigurationSqlStore<'a> {
    connection: &'a Connection,
}

#[cfg(test)]
impl<'a> ConfigurationSqlStore<'a> {
    fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }
}

#[cfg(test)]
impl ConfigurationSqlStore<'_> {
    pub async fn current_revision(
        &self,
    ) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
        let revision_id = current_revision_id_from_executor(self.connection).await?;
        read_revision_from_executor(self.connection, &revision_id)
            .await?
            .ok_or_else(|| invalid_store_data("current configuration revision disappeared"))
    }

    pub async fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>> {
        revision_id
            .validate()
            .map_err(ConfigurationStoreError::from)?;
        read_revision_from_executor(self.connection, revision_id).await
    }

    pub async fn save_change_plan(
        &self,
        plan: &ConfigurationProtectedPlanRecordV1,
    ) -> ConfigurationStoreResult<()> {
        plan.validate().map_err(ConfigurationStoreError::from)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(unavailable_store)?;
        let outcome = match read_change_plan_from_executor(&transaction, &plan.plan.plan_id).await {
            Ok(Some(existing)) if existing == *plan => Ok(()),
            Ok(Some(_)) => Err(invalid_store_data(
                "configuration change plan id conflicts with immutable prior input",
            )),
            Ok(None) => insert_change_plan(&transaction, plan).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => transaction.commit().await.map_err(unavailable_store),
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn read_change_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationStoreResult<Option<ConfigurationProtectedPlanRecordV1>> {
        plan_id.validate().map_err(ConfigurationStoreError::from)?;
        read_change_plan_from_executor(self.connection, plan_id).await
    }

    pub async fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
        validate_commit_bindings(&commit)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(unavailable_store)?;
        let outcome = commit_configuration_transaction(&transaction, &commit, false, None).await;
        match outcome {
            Ok(receipt) => transaction
                .commit()
                .await
                .map(|()| receipt)
                .map_err(unavailable_store),
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn audit(
        &self,
        after: Option<&ConfigurationAuditEventId>,
        limit: usize,
    ) -> ConfigurationStoreResult<Vec<ConfigurationAuditEvent>> {
        if limit == 0 {
            return Err(invalid_store_data(
                "configuration audit limit must be non-zero",
            ));
        }
        let limit = i64::try_from(limit).map_err(|_| {
            invalid_store_data("configuration audit limit exceeds SQLite integer range")
        })?;
        let cursor = if let Some(after) = after {
            after.validate().map_err(ConfigurationStoreError::from)?;
            let mut rows = self
                .connection
                .query(
                    "SELECT occurred_at FROM configuration_audit_events WHERE event_id = ?1",
                    params![after.as_str()],
                )
                .await
                .map_err(unavailable_store)?;
            let Some(row) = rows.next().await.map_err(unavailable_store)? else {
                return Err(invalid_store_data(
                    "configuration audit cursor does not exist",
                ));
            };
            let occurred_at = row.get::<i64>(0).map_err(|error| {
                invalid_store_data(format!("read configuration audit cursor time: {error}"))
            })?;
            Some((occurred_at, after.as_str().to_owned()))
        } else {
            None
        };
        let mut rows = match cursor {
            Some((occurred_at, event_id)) => self
                .connection
                .query(
                    "SELECT event_id, actor_id, idempotency_key, operation_kind,
                            base_revision_id, result_revision_id, sealed_target_reference,
                            event_scoped_target_commitment, receipt_digest, safe_reason_code, occurred_at
                     FROM configuration_audit_events
                     WHERE occurred_at > ?1 OR (occurred_at = ?1 AND event_id > ?2)
                     ORDER BY occurred_at ASC, event_id ASC
                     LIMIT ?3",
                    params![occurred_at, event_id, limit],
                )
                .await
                .map_err(unavailable_store)?,
            None => self
                .connection
                .query(
                    "SELECT event_id, actor_id, idempotency_key, operation_kind,
                            base_revision_id, result_revision_id, sealed_target_reference,
                            event_scoped_target_commitment, receipt_digest, safe_reason_code, occurred_at
                     FROM configuration_audit_events
                     ORDER BY occurred_at ASC, event_id ASC
                     LIMIT ?1",
                    params![limit],
                )
                .await
                .map_err(unavailable_store)?,
        };
        let mut events = Vec::new();
        while let Some(row) = rows.next().await.map_err(unavailable_store)? {
            let (event, sealed_target_reference) = decode_audit_row(&row)?;
            if sealed_target_reference.is_some() {
                return Err(invalid_store_data(
                    "connection-local test store cannot authorize sealed audit targets",
                ));
            }
            events.push(event);
        }
        Ok(events)
    }
}

#[cfg(test)]
impl ConfigurationRevisionStore for ConfigurationSqlStore<'_> {
    async fn current_revision(&self) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
        ConfigurationSqlStore::current_revision(self).await
    }

    async fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>> {
        ConfigurationSqlStore::read_revision(self, revision_id).await
    }

    async fn save_change_plan(
        &self,
        plan: &ConfigurationProtectedPlanRecordV1,
    ) -> ConfigurationStoreResult<()> {
        ConfigurationSqlStore::save_change_plan(self, plan).await
    }

    async fn read_change_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationStoreResult<Option<ConfigurationProtectedPlanRecordV1>> {
        ConfigurationSqlStore::read_change_plan(self, plan_id).await
    }

    async fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
        ConfigurationSqlStore::commit(self, commit).await
    }

    async fn audit(
        &self,
        after: Option<&ConfigurationAuditEventId>,
        limit: usize,
    ) -> ConfigurationStoreResult<Vec<ConfigurationAuditEvent>> {
        ConfigurationSqlStore::audit(self, after, limit).await
    }
}

/// Concrete control-plane adapter over one already-open owned session store.
/// It never accepts an arbitrary connection, opens a fallback database, or
/// owns policy resolution; every write obtains the selected store's serialized
/// immediate transaction and commits all durable effects together.
pub struct GlobalDbConfigurationControlStore<'db> {
    db: &'db RegisteredGlobalDb,
}

impl<'db> GlobalDbConfigurationControlStore<'db> {
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
    pub async fn initialize_canonical(
        &self,
        revision_id: &ConfigurationRevisionId,
        resolution: &ConfigurationResolutionV1,
        occurred_at: UtcMicros,
    ) -> Result<(), ConfigurationError> {
        revision_id
            .validate()
            .map_err(ConfigurationError::validation)?;
        resolution
            .snapshot
            .validate()
            .map_err(ConfigurationError::validation)?;
        validate_snapshot_registry_completeness(&resolution.snapshot).map_err(map_store_error)?;
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
            insert_revision(&transaction, &revision)
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

    /// Republishes the daemon-owned project source binding with the locator
    /// digest of a moved or renamed checkout whose identity the registry has
    /// already re-verified.
    ///
    /// This is a daemon-owned identity-preserving heal, not an operator scope
    /// change: the caller must have already resolved `binding.authority` to
    /// the exact registered project for the current root, and the stored
    /// binding must match on binding id, source kind, and authority so only
    /// the derived locator digest changes. The write is a compare-and-swap
    /// against the revision the caller read; concurrent mutation surfaces as
    /// a typed `RevisionConflict` and the caller re-reads.
    pub async fn rebind_daemon_project_source_binding(
        &self,
        expected_revision_id: &ConfigurationRevisionId,
        binding: &ScopeSourceBinding,
        occurred_at: UtcMicros,
    ) -> Result<ConfigurationCurrentStateV1, ConfigurationError> {
        expected_revision_id
            .validate()
            .map_err(ConfigurationError::validation)?;
        binding.validate().map_err(ConfigurationError::validation)?;
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
            let operation_digest = canonical_sha256(&(
                "tracedecay.configuration.daemon-source-binding-rebind.v1",
                binding,
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
                "configuration rebind revision id",
            )?;
            let snapshot = current
                .snapshot
                .apply_protected_change(
                    &ProtectedChange::RebindSource(binding.clone()),
                    &next_revision_id,
                )
                .map_err(map_protected_change_snapshot_error)?;
            validate_snapshot_registry_completeness(&snapshot).map_err(map_store_error)?;
            let actor_id = ActorId::new("actor.tracedecay-daemon.source-binding-rebind".to_owned())
                .map_err(ConfigurationError::validation)?;
            let revision = ConfigurationRevisionRecordV1 {
                revision_id: next_revision_id.clone(),
                parent_revision_id: Some(expected_revision_id.clone()),
                snapshot,
                actor_id,
                operation_kind: "daemon_source_binding_rebind".to_owned(),
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

    /// Revalidate the current daemon/maintenance scope before any mutation.
    ///
    /// The retained registered database may outlive its admission scope. Use
    /// the runtime-core authority check against the exact database path rather
    /// than opening or shadowing another database handle.
    fn require_active_mutation_scope(db: &RegisteredGlobalDb) -> Result<(), ConfigurationError> {
        tracedecay_runtime_core::db::DatabaseAuthority::for_owned_runtime(
            db.db_path(),
            "configuration control mutation",
        )
        .map(|_| ())
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
            Self::require_active_mutation_scope(db.as_ref())?;
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
            OwnedGlobalDbConfigurationControlStore::require_active_mutation_scope(db.as_ref())?;
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

impl CredentialWritePort for OwnedGlobalDbConfigurationControlStore {
    fn write_reference(
        &self,
        authority: &ConfigurationMutationAuthority,
        write: &WriteOnlyCredentialMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, CredentialReferenceMetadataV1> {
        forward_to_registered!(
            self,
            [authority, write, expected_revision],
            mutating,
            |store| store.write_reference(&authority, &write, &expected_revision)
        )
    }
}

#[cfg(test)]
mod tests;

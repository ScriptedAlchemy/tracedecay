use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ManifestDigest, UtcMicros};

use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, RetrievalProfileCasV1, RetrievalProfileCommitMetadataV1,
    RetrievalProfileMutationCapabilityV1, RetrievalProfileStateSnapshotV1, RetrievalProfileStateV1,
    RetrievalRuntimeCompatibilityV1,
};
use crate::configuration::{ConfigurationMutationAuthority, DirectConfigurationMutation};
use crate::semantic_runtime::{
    CommittedRetrievalProfileStateV1, SemanticActivationCommandV1, SemanticActivationReceiptV1,
    SemanticConfigurationBackendErrorV1, SemanticConfigurationPinV1,
    SemanticConfigurationTransitionV1, SemanticCurrentLinkedActivationV1,
    SemanticLinkedTransitionV1, SemanticRetrievalConfigurationPortV1, SemanticRollbackCommandV1,
    SemanticRuntimeFuture,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS configuration_semantic_retrieval_state_v1 (
    scope_digest TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK (epoch >= 0),
    configuration_revision TEXT NOT NULL,
    transition_digest TEXT,
    activation_receipt_digest TEXT,
    state_json TEXT NOT NULL,
    activation_receipt_json TEXT,
    PRIMARY KEY (scope_digest, epoch)
);
CREATE UNIQUE INDEX IF NOT EXISTS configuration_semantic_retrieval_transition_v1
    ON configuration_semantic_retrieval_state_v1(scope_digest, transition_digest)
    WHERE transition_digest IS NOT NULL;
CREATE TABLE IF NOT EXISTS configuration_semantic_retrieval_pending_v1 (
    scope_digest TEXT NOT NULL,
    transition_digest TEXT NOT NULL,
    base_epoch INTEGER NOT NULL CHECK (base_epoch >= 0),
    base_configuration_revision TEXT NOT NULL,
    transition_json TEXT NOT NULL,
    resulting_state_json TEXT NOT NULL,
    staged_at INTEGER NOT NULL,
    PRIMARY KEY (scope_digest, transition_digest)
);";

#[derive(Clone)]
pub struct ProductionSemanticRetrievalConfigurationStoreV1 {
    database: Arc<RegisteredGlobalDb>,
    scope: ResolvedScope,
    prepared_central_commits: Arc<Mutex<BTreeMap<String, PreparedCentralCommit>>>,
}

impl ProductionSemanticRetrievalConfigurationStoreV1 {
    pub async fn open(
        database: Arc<RegisteredGlobalDb>,
        scope: ResolvedScope,
    ) -> Result<Self, SemanticConfigurationBackendErrorV1> {
        scope
            .validate()
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        let store = Self {
            database,
            scope,
            prepared_central_commits: Arc::new(Mutex::new(BTreeMap::new())),
        };
        store.ensure_schema().await?;
        Ok(store)
    }

    pub async fn install_initial_state(
        &self,
        configuration: &SemanticConfigurationPinV1,
        state: &RetrievalProfileStateV1,
    ) -> Result<(), SemanticConfigurationBackendErrorV1> {
        configuration
            .validate()
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        if state.configuration_revision() != &configuration.revision_id || !state.audit().is_empty()
        {
            return Err(SemanticConfigurationBackendErrorV1::Rejected);
        }
        self.ensure_schema().await?;
        let state_json = encode_state(state)?;
        let transaction = self
            .database
            .begin_write_transaction()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let existing = load_latest(&transaction, &self.scope.scope_digest).await?;
        if let Some(existing) = existing {
            return if existing.state == state.clone() && existing.receipt.is_none() {
                Ok(())
            } else {
                Err(SemanticConfigurationBackendErrorV1::Conflict)
            };
        }
        transaction
            .execute(
                "INSERT INTO configuration_semantic_retrieval_state_v1 (
                    scope_digest, epoch, configuration_revision, transition_digest,
                    activation_receipt_digest, state_json, activation_receipt_json
                 ) VALUES (?1, 0, ?2, NULL, NULL, ?3, NULL)",
                params![
                    self.scope.scope_digest.as_str(),
                    configuration.revision_id.as_str(),
                    state_json
                ],
            )
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Conflict)?;
        transaction
            .commit()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)
    }

    pub async fn current_committed_state(
        &self,
    ) -> Result<Option<CommittedRetrievalProfileStateV1>, SemanticConfigurationBackendErrorV1> {
        self.ensure_schema().await?;
        let snapshot = self
            .database
            .read_snapshot()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let Some(stored) = load_latest(&snapshot, &self.scope.scope_digest).await? else {
            return Ok(None);
        };
        if stored.transition_digest.is_none() {
            return Ok(None);
        }
        let current_activation =
            current_activation_from_stored(&stored, stored.state.configuration_revision())?;
        if stored.activation_receipt_digest.as_ref()
            != current_activation
                .as_ref()
                .map(|activation| &activation.receipt.receipt_digest)
        {
            return Err(SemanticConfigurationBackendErrorV1::Rejected);
        }
        Ok(Some(CommittedRetrievalProfileStateV1 {
            scope: self.scope.clone(),
            state: stored.state,
            current_activation,
        }))
    }

    pub async fn current_state_if_present(
        &self,
    ) -> Result<Option<RetrievalProfileStateV1>, SemanticConfigurationBackendErrorV1> {
        self.ensure_schema().await?;
        let snapshot = self
            .database
            .read_snapshot()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        Ok(load_latest(&snapshot, &self.scope.scope_digest)
            .await?
            .map(|stored| stored.state))
    }

    pub async fn current_profile_state(
        &self,
    ) -> Result<RetrievalProfileStateSnapshotV1, SemanticConfigurationBackendErrorV1> {
        self.current_record()
            .await?
            .state
            .snapshot()
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)
    }

    pub(crate) async fn preview_central_mutation(
        &self,
        authority: &ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &tracedecay_domain::ConfigurationRevisionId,
    ) -> Result<
        tracedecay_global_db::configuration::store::ConfigurationDirectCommitOutcomeV1,
        SemanticConfigurationBackendErrorV1,
    > {
        let transaction = self
            .database
            .begin_write_transaction()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let preview = tracedecay_global_db::configuration::store::commit_direct_in_transaction(
            &transaction,
            authority,
            mutation,
            expected_revision,
        )
        .await
        .map_err(|error| match error {
            crate::configuration::ConfigurationError::RevisionConflict => {
                SemanticConfigurationBackendErrorV1::Conflict
            }
            crate::configuration::ConfigurationError::Unavailable => {
                SemanticConfigurationBackendErrorV1::Unavailable
            }
            _ => SemanticConfigurationBackendErrorV1::Rejected,
        })?;
        drop(transaction);
        Ok(preview)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stage_activation(
        &self,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: SemanticConfigurationPinV1,
        capability: &RetrievalProfileMutationCapabilityV1,
        expected: RetrievalProfileCasV1,
        candidate: AcceptedRetrievalProfileV1,
        current_runtime: &RetrievalRuntimeCompatibilityV1,
        candidate_runtime: &RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1> {
        let stored = self.current_record().await?;
        if stored.state.configuration_revision() != &base_configuration.revision_id
            || result_configuration.revision_id == base_configuration.revision_id
        {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        let prior_active = stored.state.active().clone();
        let prior_active_semantic = prior_active.compatibility().semantic.clone();
        let prior_rollback_semantic = stored
            .state
            .rollback_profile()
            .and_then(|profile| profile.compatibility().semantic.clone());
        let mut resulting = stored.state.clone();
        resulting
            .activate(
                capability,
                &expected,
                candidate.clone(),
                current_runtime,
                candidate_runtime,
                RetrievalProfileCommitMetadataV1::new(
                    freshness_vector_digest,
                    result_configuration.revision_id.clone(),
                    now,
                ),
            )
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        let transition = SemanticConfigurationTransitionV1::activation(
            base_configuration,
            result_configuration,
            prior_active.profile().profile_id.clone(),
            &candidate,
            candidate_runtime,
            expected,
            prior_active_semantic,
            prior_rollback_semantic,
            now,
        )
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        self.remember_central_commit(
            &transition,
            capability.authority().clone(),
            central_mutation,
        )?;
        self.persist_pending(stored.epoch, &transition, &resulting)
            .await?;
        Ok(transition)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stage_rollback(
        &self,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: SemanticConfigurationPinV1,
        capability: &RetrievalProfileMutationCapabilityV1,
        expected: RetrievalProfileCasV1,
        restored_runtime: &RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        trigger: String,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1> {
        let stored = self.current_record().await?;
        let restored = stored
            .state
            .rollback_profile()
            .cloned()
            .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
        let prior_active = stored.state.active().clone();
        let prior_active_semantic = prior_active
            .compatibility()
            .semantic
            .clone()
            .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
        let prior_rollback_semantic = restored.compatibility().semantic.clone();
        let mut resulting = stored.state.clone();
        resulting
            .rollback(
                capability,
                &expected,
                restored_runtime,
                trigger.clone(),
                RetrievalProfileCommitMetadataV1::new(
                    freshness_vector_digest,
                    result_configuration.revision_id.clone(),
                    now,
                ),
            )
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        let transition = SemanticConfigurationTransitionV1::rollback(
            base_configuration,
            result_configuration,
            prior_active.profile().profile_id.clone(),
            &restored,
            restored_runtime,
            expected,
            prior_active_semantic,
            prior_rollback_semantic,
            trigger,
            now,
        )
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        self.remember_central_commit(
            &transition,
            capability.authority().clone(),
            central_mutation,
        )?;
        self.persist_pending(stored.epoch, &transition, &resulting)
            .await?;
        Ok(transition)
    }

    async fn ensure_schema(&self) -> Result<(), SemanticConfigurationBackendErrorV1> {
        self.database
            .writer_connection()
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
            .execute_batch(SCHEMA)
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)
    }

    async fn current_record(&self) -> Result<StoredState, SemanticConfigurationBackendErrorV1> {
        self.ensure_schema().await?;
        let snapshot = self
            .database
            .read_snapshot()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        load_latest(&snapshot, &self.scope.scope_digest)
            .await?
            .ok_or(SemanticConfigurationBackendErrorV1::Unavailable)
    }

    async fn persist_pending(
        &self,
        base_epoch: i64,
        transition: &SemanticConfigurationTransitionV1,
        resulting: &RetrievalProfileStateV1,
    ) -> Result<(), SemanticConfigurationBackendErrorV1> {
        transition
            .validate()
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        let transition_json = serde_json::to_string(transition)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
        let resulting_state_json = encode_state(resulting)?;
        let transaction = self
            .database
            .begin_write_transaction()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let current = load_latest(&transaction, &self.scope.scope_digest)
            .await?
            .ok_or(SemanticConfigurationBackendErrorV1::Unavailable)?;
        if current.epoch != base_epoch
            || current.state.configuration_revision() != &transition.base_configuration.revision_id
        {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        transaction
            .execute(
                "INSERT INTO configuration_semantic_retrieval_pending_v1 (
                    scope_digest, transition_digest, base_epoch, base_configuration_revision,
                    transition_json, resulting_state_json, staged_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(scope_digest, transition_digest) DO NOTHING",
                params![
                    self.scope.scope_digest.as_str(),
                    transition.transition_digest.as_str(),
                    base_epoch,
                    transition.base_configuration.revision_id.as_str(),
                    transition_json,
                    resulting_state_json,
                    transition.transition_at.0,
                ],
            )
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Conflict)?;
        transaction
            .commit()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)
    }

    fn remember_central_commit(
        &self,
        transition: &SemanticConfigurationTransitionV1,
        authority: ConfigurationMutationAuthority,
        mutation: DirectConfigurationMutation,
    ) -> Result<(), SemanticConfigurationBackendErrorV1> {
        let prepared = PreparedCentralCommit {
            authority,
            mutation,
        };
        let mut commits = self
            .prepared_central_commits
            .lock()
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        match commits.get(transition.transition_digest.as_str()) {
            Some(existing) if existing != &prepared => {
                Err(SemanticConfigurationBackendErrorV1::Conflict)
            }
            Some(_) => Ok(()),
            None => {
                commits.insert(transition.transition_digest.as_str().to_owned(), prepared);
                Ok(())
            }
        }
    }

    pub async fn current_committed_profile_state(
        &self,
        configuration: &SemanticConfigurationPinV1,
    ) -> Result<CommittedRetrievalProfileStateV1, SemanticConfigurationBackendErrorV1> {
        let stored = self.current_record().await?;
        if stored.state.configuration_revision() != &configuration.revision_id {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        let current_activation =
            current_activation_from_stored(&stored, &configuration.revision_id)?;
        Ok(CommittedRetrievalProfileStateV1 {
            scope: self.scope.clone(),
            state: stored.state,
            current_activation,
        })
    }
}

impl SemanticRetrievalConfigurationPortV1 for ProductionSemanticRetrievalConfigurationStoreV1 {
    fn current_activation<'a>(
        &'a self,
        configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<Option<SemanticCurrentLinkedActivationV1>, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async move {
            let stored = self.current_record().await?;
            if stored.state.configuration_revision() != &configuration.revision_id {
                return Err(SemanticConfigurationBackendErrorV1::Conflict);
            }
            current_activation_from_stored(&stored, &configuration.revision_id)
        })
    }

    fn prepare_activation<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async move { self.load_pending_for_activation(command).await })
    }

    fn prepare_rollback<'a>(
        &'a self,
        command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async move { self.load_pending_for_rollback(command).await })
    }

    fn commit_linked_transition<'a>(
        &'a self,
        transition: &'a SemanticConfigurationTransitionV1,
        receipt: Option<&'a SemanticActivationReceiptV1>,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticLinkedTransitionV1, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async move {
            let prepared = self
                .prepared_central_commits
                .lock()
                .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
                .get(transition.transition_digest.as_str())
                .cloned()
                .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
            let transaction = self
                .database
                .begin_write_transaction()
                .await
                .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
            let current = load_latest(&transaction, &self.scope.scope_digest)
                .await?
                .ok_or(SemanticConfigurationBackendErrorV1::Unavailable)?;
            let pending = load_pending(
                &transaction,
                &self.scope.scope_digest,
                &transition.transition_digest,
            )
            .await?;
            if current.epoch != pending.base_epoch
                || current.state.configuration_revision()
                    != &transition.base_configuration.revision_id
                || pending.transition != *transition
            {
                return Err(SemanticConfigurationBackendErrorV1::Conflict);
            }
            let central = tracedecay_global_db::configuration::store::commit_direct_in_transaction(
                &transaction,
                &prepared.authority,
                &prepared.mutation,
                &transition.base_configuration.revision_id,
            )
            .await
            .map_err(|error| match error {
                crate::configuration::ConfigurationError::RevisionConflict => {
                    SemanticConfigurationBackendErrorV1::Conflict
                }
                crate::configuration::ConfigurationError::Unavailable => {
                    SemanticConfigurationBackendErrorV1::Unavailable
                }
                _ => SemanticConfigurationBackendErrorV1::Rejected,
            })?;
            let committed_configuration =
                SemanticConfigurationPinV1::from_current(&central.current)
                    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            if committed_configuration != transition.result_configuration
                || central.receipt.base_revision_id != transition.base_configuration.revision_id
                || central.receipt.result_revision_id != transition.result_configuration.revision_id
            {
                return Err(SemanticConfigurationBackendErrorV1::Rejected);
            }
            let audit = pending
                .resulting_state
                .audit()
                .last()
                .cloned()
                .ok_or(SemanticConfigurationBackendErrorV1::Rejected)?;
            let linked = SemanticLinkedTransitionV1::new(transition, receipt, audit)
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            let state_json = encode_state(&pending.resulting_state)?;
            let receipt_json = receipt
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            let receipt_digest = receipt.map(|receipt| receipt.receipt_digest.as_str());
            transaction
                .execute(
                    "INSERT INTO configuration_semantic_retrieval_state_v1 (
                        scope_digest, epoch, configuration_revision, transition_digest,
                        activation_receipt_digest, state_json, activation_receipt_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        self.scope.scope_digest.as_str(),
                        current.epoch + 1,
                        transition.result_configuration.revision_id.as_str(),
                        transition.transition_digest.as_str(),
                        receipt_digest,
                        state_json,
                        receipt_json,
                    ],
                )
                .await
                .map_err(|_| SemanticConfigurationBackendErrorV1::Conflict)?;
            transaction
                .commit()
                .await
                .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
            if let Ok(mut commits) = self.prepared_central_commits.lock() {
                commits.remove(transition.transition_digest.as_str());
            }
            Ok(linked)
        })
    }

    fn committed_profile_state<'a>(
        &'a self,
        linked: &'a SemanticLinkedTransitionV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<CommittedRetrievalProfileStateV1, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async move {
            let stored = self.current_record().await?;
            if stored.transition_digest.as_ref() != Some(&linked.transition_digest)
                || stored.activation_receipt_digest.as_ref()
                    != linked.activation_receipt_digest.as_ref()
            {
                return Err(SemanticConfigurationBackendErrorV1::Conflict);
            }
            let current_activation =
                current_activation_from_stored(&stored, stored.state.configuration_revision())?;
            let committed = CommittedRetrievalProfileStateV1 {
                scope: self.scope.clone(),
                state: stored.state,
                current_activation,
            };
            committed
                .validate_for(linked)
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            Ok(committed)
        })
    }
}

impl ProductionSemanticRetrievalConfigurationStoreV1 {
    async fn load_pending_for_activation(
        &self,
        command: &SemanticActivationCommandV1,
    ) -> Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1> {
        let pending = self.load_latest_pending().await?;
        if pending.transition.base_configuration != command.configuration
            || pending
                .transition
                .result_active_semantic
                .as_ref()
                .map(|pins| &pins.vector_generation_id)
                != Some(&command.request.target_generation)
            || pending
                .transition
                .prior_active_semantic
                .as_ref()
                .map(|pins| &pins.vector_generation_id)
                != command.request.expected_active_generation.as_ref()
            || pending
                .transition
                .prior_rollback_semantic
                .as_ref()
                .map(|pins| &pins.vector_generation_id)
                != command.request.expected_rollback_generation.as_ref()
            || !matches!(
                pending.transition.operation,
                crate::config::retrieval::RetrievalProfileAuditOperationV1::Activate
            )
        {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        Ok(pending.transition)
    }

    async fn load_pending_for_rollback(
        &self,
        command: &SemanticRollbackCommandV1,
    ) -> Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1> {
        let pending = self.load_latest_pending().await?;
        if pending.transition.base_configuration != command.configuration
            || pending
                .transition
                .result_active_semantic
                .as_ref()
                .map(|pins| &pins.vector_generation_id)
                != command.request.target_generation.as_ref()
            || !matches!(
                pending.transition.operation,
                crate::config::retrieval::RetrievalProfileAuditOperationV1::Rollback { .. }
            )
        {
            return Err(SemanticConfigurationBackendErrorV1::Conflict);
        }
        Ok(pending.transition)
    }

    async fn load_latest_pending(
        &self,
    ) -> Result<PendingTransition, SemanticConfigurationBackendErrorV1> {
        self.ensure_schema().await?;
        let snapshot = self
            .database
            .read_snapshot()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let mut rows = snapshot
            .query(
                "SELECT scope_digest, transition_digest, base_epoch,
                        transition_json, resulting_state_json
                 FROM configuration_semantic_retrieval_pending_v1
                 WHERE scope_digest = ?1
                 ORDER BY staged_at DESC, rowid DESC
                 LIMIT 1",
                params![self.scope.scope_digest.as_str()],
            )
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
            .ok_or(SemanticConfigurationBackendErrorV1::Unavailable)?;
        decode_pending_row(&row, &self.scope.scope_digest)
    }
}

#[derive(Clone, Debug)]
struct StoredState {
    epoch: i64,
    state: RetrievalProfileStateV1,
    receipt: Option<SemanticActivationReceiptV1>,
    transition_digest: Option<ManifestDigest>,
    activation_receipt_digest: Option<ManifestDigest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparedCentralCommit {
    authority: ConfigurationMutationAuthority,
    mutation: DirectConfigurationMutation,
}

fn current_activation_from_stored(
    stored: &StoredState,
    configuration_revision: &tracedecay_domain::ConfigurationRevisionId,
) -> Result<Option<SemanticCurrentLinkedActivationV1>, SemanticConfigurationBackendErrorV1> {
    match (
        stored.state.active().compatibility().semantic.as_ref(),
        stored.receipt.as_ref(),
    ) {
        (Some(compatibility), Some(receipt)) => {
            if &receipt.configuration.revision_id != configuration_revision {
                return Err(SemanticConfigurationBackendErrorV1::Conflict);
            }
            SemanticCurrentLinkedActivationV1::new(receipt.clone(), compatibility.clone())
                .map(Some)
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)
        }
        (None, None) => Ok(None),
        _ => Err(SemanticConfigurationBackendErrorV1::Rejected),
    }
}

#[derive(Clone, Debug)]
struct PendingTransition {
    base_epoch: i64,
    transition: SemanticConfigurationTransitionV1,
    resulting_state: RetrievalProfileStateV1,
}

async fn load_latest<E>(
    executor: &E,
    scope_digest: &ManifestDigest,
) -> Result<Option<StoredState>, SemanticConfigurationBackendErrorV1>
where
    E: QueryExecutor + Sync,
{
    let mut rows = executor
        .query(
            "SELECT scope_digest, epoch, state_json, activation_receipt_json,
                    transition_digest, activation_receipt_digest
             FROM configuration_semantic_retrieval_state_v1
             WHERE scope_digest = ?1
             ORDER BY epoch DESC
             LIMIT 1",
            params![scope_digest.as_str()],
        )
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
    else {
        return Ok(None);
    };
    let stored_scope = ManifestDigest::new(
        row.get::<String>(0)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )
    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    if &stored_scope != scope_digest {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    let epoch = row
        .get::<i64>(1)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let state_json = row
        .get::<String>(2)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let receipt_json = row
        .get::<Option<String>>(3)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let transition_digest = decode_optional_digest(
        row.get::<Option<String>>(4)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )?;
    let activation_receipt_digest = decode_optional_digest(
        row.get::<Option<String>>(5)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )?;
    let state = decode_state(&state_json)?;
    let receipt = receipt_json
        .map(|json| {
            let receipt: SemanticActivationReceiptV1 = serde_json::from_str(&json)
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            receipt
                .validate()
                .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
            Ok(receipt)
        })
        .transpose()?;
    if receipt.as_ref().map(|receipt| &receipt.receipt_digest) != activation_receipt_digest.as_ref()
    {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    Ok(Some(StoredState {
        epoch,
        state,
        receipt,
        transition_digest,
        activation_receipt_digest,
    }))
}

async fn load_pending<E>(
    executor: &E,
    scope_digest: &ManifestDigest,
    digest: &ManifestDigest,
) -> Result<PendingTransition, SemanticConfigurationBackendErrorV1>
where
    E: QueryExecutor + Sync,
{
    let mut rows = executor
        .query(
            "SELECT scope_digest, transition_digest, base_epoch,
                    transition_json, resulting_state_json
             FROM configuration_semantic_retrieval_pending_v1
             WHERE scope_digest = ?1 AND transition_digest = ?2",
            params![scope_digest.as_str(), digest.as_str()],
        )
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| SemanticConfigurationBackendErrorV1::Unavailable)?
        .ok_or(SemanticConfigurationBackendErrorV1::Unavailable)?;
    decode_pending_row(&row, scope_digest)
}

fn decode_pending_row(
    row: &tracedecay_runtime_core::db::engine::Row,
    expected_scope: &ManifestDigest,
) -> Result<PendingTransition, SemanticConfigurationBackendErrorV1> {
    let stored_scope = ManifestDigest::new(
        row.get::<String>(0)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )
    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    if &stored_scope != expected_scope {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    let stored_digest = ManifestDigest::new(
        row.get::<String>(1)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )
    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let base_epoch = row
        .get::<i64>(2)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    let transition: SemanticConfigurationTransitionV1 = serde_json::from_str(
        &row.get::<String>(3)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )
    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?;
    if transition.transition_digest != stored_digest {
        return Err(SemanticConfigurationBackendErrorV1::Rejected);
    }
    let resulting_state = decode_state(
        &row.get::<String>(4)
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )?;
    Ok(PendingTransition {
        base_epoch,
        transition,
        resulting_state,
    })
}

fn encode_state(
    state: &RetrievalProfileStateV1,
) -> Result<String, SemanticConfigurationBackendErrorV1> {
    serde_json::to_string(
        &state
            .snapshot()
            .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?,
    )
    .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)
}

fn decode_state(
    json: &str,
) -> Result<RetrievalProfileStateV1, SemanticConfigurationBackendErrorV1> {
    serde_json::from_str::<RetrievalProfileStateSnapshotV1>(json)
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)?
        .into_state()
        .map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)
}

fn decode_optional_digest(
    value: Option<String>,
) -> Result<Option<ManifestDigest>, SemanticConfigurationBackendErrorV1> {
    value
        .map(|value| {
            ManifestDigest::new(value).map_err(|_| SemanticConfigurationBackendErrorV1::Rejected)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use tracedecay_application::ResolvedScope;
    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};

    use super::*;
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;

    #[tokio::test]
    async fn exact_query_bootstrap_is_explicit_and_required_before_activation() {
        let directory = tempfile::tempdir().unwrap();
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let project_id = ProjectId::new("project.semantic-bootstrap").unwrap();
        let runtime = RegisteredGlobalDbTestRuntime::project(
            directory.path().join("profile"),
            &project_root,
            project_id.clone(),
        )
        .await
        .unwrap();
        let database = runtime.project_database_arc().unwrap();
        let scope = ResolvedScope::new(
            project_id,
            RepositoryId::new("repository.semantic-bootstrap").unwrap(),
            WorktreeId::new("worktree.semantic-bootstrap").unwrap(),
            None,
        )
        .unwrap();
        let store = ProductionSemanticRetrievalConfigurationStoreV1::open(database, scope)
            .await
            .unwrap();
        assert_eq!(
            store.current_record().await.unwrap_err(),
            SemanticConfigurationBackendErrorV1::Unavailable
        );

        let (_, accepted, compatibility) =
            crate::semantic_runtime::bundled_query_authority().unwrap();
        assert!(accepted.is_exact_query_fallback());
        let configuration = SemanticConfigurationPinV1 {
            revision_id: ConfigurationRevisionId::new("configuration.revision.semantic-bootstrap")
                .unwrap(),
            snapshot_id: ConfigurationSnapshotId::new("configuration.snapshot.semantic-bootstrap")
                .unwrap(),
            effective_behavior_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                .unwrap(),
        };
        let state = RetrievalProfileStateV1::new(
            configuration.revision_id.clone(),
            accepted.clone(),
            &compatibility,
        )
        .unwrap();

        store
            .install_initial_state(&configuration, &state)
            .await
            .unwrap();

        let stored = store.current_record().await.unwrap();
        assert_eq!(stored.state.active(), &accepted);
        assert!(stored.state.active().is_exact_query_fallback());
        assert!(stored.receipt.is_none());
    }
}

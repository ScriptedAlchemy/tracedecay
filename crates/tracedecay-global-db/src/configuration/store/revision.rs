//! Revision write and control-store persistence boundaries.

use std::future::Future;

use super::audit::audit_from_transaction;
use super::codec::insert_configuration_projections;
use super::mutation::{commit_configuration_transaction, validate_commit_bindings};
use super::read::{
    current_revision_id_from_executor, read_change_plan_from_executor, read_revision_from_executor,
    validate_snapshot_registry_completeness,
};
use super::write::{insert_change_plan, insert_snapshot_entries};
use super::{
    ChangePlanId, ConfigurationAuditEvent, ConfigurationAuditEventId, ConfigurationCommitV1,
    ConfigurationMutationReceiptV1, ConfigurationProtectedPlanRecordV1, ConfigurationRevisionId,
    ConfigurationRevisionRecordV1, ConfigurationRevisionStore, ConfigurationStoreError,
    ConfigurationStoreResult, Executor, GlobalDbConfigurationControlStore, invalid_store_data,
    params, unavailable_store,
};

pub(super) async fn insert_revision(
    transaction: &impl Executor,
    revision: &ConfigurationRevisionRecordV1,
) -> ConfigurationStoreResult<()> {
    revision.validate().map_err(ConfigurationStoreError::from)?;
    validate_snapshot_registry_completeness(&revision.snapshot)?;
    let parent_revision_id = revision
        .parent_revision_id
        .as_ref()
        .map(|value| value.as_str().to_owned());
    transaction
        .execute(
            "INSERT INTO configuration_revisions (
                revision_id, parent_revision_id, snapshot_id,
                effective_behavior_digest, resolution_provenance_digest,
                actor_id, operation_kind, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                revision.revision_id.as_str(),
                parent_revision_id,
                revision.snapshot.snapshot_id.as_str(),
                revision.snapshot.effective_behavior_digest.as_str(),
                revision.snapshot.resolution_provenance_digest.as_str(),
                revision.actor_id.as_str(),
                revision.operation_kind.as_str(),
                revision.created_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    insert_snapshot_entries(transaction, &revision.revision_id, &revision.snapshot).await?;
    insert_configuration_projections(transaction, &revision.revision_id, &revision.snapshot).await
}

impl ConfigurationRevisionStore for GlobalDbConfigurationControlStore<'_> {
    async fn current_revision(&self) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
        let read = self.db.read_snapshot().await.map_err(unavailable_store)?;
        let revision_id = current_revision_id_from_executor(&read).await?;
        read_revision_from_executor(&read, &revision_id)
            .await?
            .ok_or_else(|| invalid_store_data("current configuration revision disappeared"))
    }

    fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> impl Future<Output = ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>>> + Send
    {
        let revision_id = revision_id.clone();
        async move {
            revision_id
                .validate()
                .map_err(ConfigurationStoreError::from)?;
            let read = self.db.read_snapshot().await.map_err(unavailable_store)?;
            read_revision_from_executor(&read, &revision_id).await
        }
    }

    fn save_change_plan(
        &self,
        plan: &ConfigurationProtectedPlanRecordV1,
    ) -> impl Future<Output = ConfigurationStoreResult<()>> + Send {
        let plan = plan.clone();
        async move {
            plan.validate().map_err(ConfigurationStoreError::from)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(unavailable_store)?;
            let outcome =
                match read_change_plan_from_executor(&transaction, &plan.plan.plan_id).await {
                    Ok(Some(existing)) if existing == plan => Ok(()),
                    Ok(Some(_)) => Err(ConfigurationStoreError::IdempotencyConflict),
                    Ok(None) => insert_change_plan(&transaction, &plan).await,
                    Err(error) => Err(error),
                };
            match outcome {
                Ok(()) => transaction.commit().await.map_err(unavailable_store),
                Err(error) => Err(error),
            }
        }
    }

    fn read_change_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> impl Future<Output = ConfigurationStoreResult<Option<ConfigurationProtectedPlanRecordV1>>> + Send
    {
        let plan_id = plan_id.clone();
        async move {
            plan_id.validate().map_err(ConfigurationStoreError::from)?;
            let read = self.db.read_snapshot().await.map_err(unavailable_store)?;
            read_change_plan_from_executor(&read, &plan_id).await
        }
    }

    async fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
        validate_commit_bindings(&commit)?;
        let transaction = self
            .db
            .begin_write_transaction()
            .await
            .map_err(unavailable_store)?;
        let outcome = commit_configuration_transaction(&transaction, &commit, false, None).await;
        match outcome {
            Ok(receipt) => transaction
                .commit()
                .await
                .map(|()| receipt)
                .map_err(unavailable_store),
            Err(error) => Err(error),
        }
    }

    fn audit(
        &self,
        after: Option<&ConfigurationAuditEventId>,
        limit: usize,
    ) -> impl Future<Output = ConfigurationStoreResult<Vec<ConfigurationAuditEvent>>> + Send {
        let after = after.cloned();
        async move {
            let read = self.db.read_snapshot().await.map_err(unavailable_store)?;
            audit_from_transaction(&read, after.as_ref(), limit).await
        }
    }
}

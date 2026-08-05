use super::codec::{insert_configuration_projections, invalid_store_data, unavailable_store};
use super::mutation::map_store_error;
use super::read::validate_snapshot_registry_completeness;
use super::write::insert_snapshot_entries;
use super::{
    ConfigurationError, ConfigurationResolutionV1, ConfigurationRevisionId, Executor,
    QueryExecutor, UtcMicros, params,
};

pub(super) async fn commit_canonical_genesis_transaction(
    transaction: &impl Executor,
    revision_id: &ConfigurationRevisionId,
    resolution: &ConfigurationResolutionV1,
    created_at: UtcMicros,
) -> Result<(), ConfigurationError> {
    validate_snapshot_registry_completeness(&resolution.snapshot).map_err(map_store_error)?;
    let mut rows = transaction
        .query("SELECT COUNT(*) FROM configuration_revisions", ())
        .await
        .map_err(|error| map_store_error(unavailable_store(error)))?;
    let revision_count = rows
        .next()
        .await
        .map_err(|error| map_store_error(unavailable_store(error)))?
        .ok_or_else(|| {
            map_store_error(invalid_store_data(
                "configuration revision count disappeared",
            ))
        })?
        .get::<i64>(0)
        .map_err(|error| map_store_error(unavailable_store(error)))?;
    if revision_count != 0 {
        return Err(ConfigurationError::validation_message(
            "canonical configuration genesis requires an empty revision store",
        ));
    }
    transaction
        .execute(
            "INSERT INTO configuration_revisions (
                revision_id, parent_revision_id, snapshot_id,
                effective_behavior_digest, resolution_provenance_digest,
                actor_id, operation_kind, created_at
             ) VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                revision_id.as_str(),
                resolution.snapshot.snapshot_id.as_str(),
                resolution.snapshot.effective_behavior_digest.as_str(),
                resolution.snapshot.resolution_provenance_digest.as_str(),
                "actor.tracedecay-daemon",
                "canonical_genesis",
                created_at.0,
            ],
        )
        .await
        .map_err(|error| map_store_error(unavailable_store(error)))?;
    insert_snapshot_entries(transaction, revision_id, &resolution.snapshot)
        .await
        .map_err(map_store_error)?;
    insert_configuration_projections(transaction, revision_id, &resolution.snapshot)
        .await
        .map_err(map_store_error)?;
    Ok(())
}

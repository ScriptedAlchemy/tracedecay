//! Desired/observed component activation state persistence.

use super::codec::unavailable_store;
use super::{
    ConfigurationRevisionId, ConfigurationStoreResult, Executor, QueryExecutor, Row, UtcMicros,
    invalid_store_data, params,
};
use tracedecay_rusqlite_runtime::repository::{
    ADVANCE_COMPONENT_DESIRED_STATE_SQL, LATEST_COMPONENT_ACTIVATION_STATES_SQL,
};
pub(super) use tracedecay_rusqlite_runtime::repository::{
    StoredComponentActivationState, validate_activation_error_code, validate_component_name,
};

pub(super) fn decode_component_activation_state(
    row: &Row,
) -> ConfigurationStoreResult<StoredComponentActivationState> {
    let component = row
        .get::<String>(0)
        .map_err(|error| invalid_store_data(format!("read configuration component: {error}")))?;
    let desired_revision_id = row.get::<String>(1).map_err(|error| {
        invalid_store_data(format!("read desired configuration revision: {error}"))
    })?;
    let observed_revision_id = row.get::<Option<String>>(2).map_err(|error| {
        invalid_store_data(format!("read observed configuration revision: {error}"))
    })?;
    let last_working_revision_id = row.get::<Option<String>>(3).map_err(|error| {
        invalid_store_data(format!("read last working configuration revision: {error}"))
    })?;
    let restart_required = row.get::<i64>(4).map_err(|error| {
        invalid_store_data(format!("read configuration restart state: {error}"))
    })?;
    let activation_error_code = row
        .get::<Option<String>>(5)
        .map_err(|error| invalid_store_data(format!("read activation error code: {error}")))?;
    StoredComponentActivationState::from_columns(
        component,
        desired_revision_id,
        observed_revision_id,
        last_working_revision_id,
        restart_required,
        activation_error_code,
    )
    .map_err(invalid_store_data)
}

pub(super) async fn latest_component_activation_states(
    transaction: &impl QueryExecutor,
) -> ConfigurationStoreResult<Vec<StoredComponentActivationState>> {
    let mut rows = transaction
        .query(LATEST_COMPONENT_ACTIVATION_STATES_SQL, ())
        .await
        .map_err(unavailable_store)?;
    let mut states = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        states.push(decode_component_activation_state(&row)?);
    }
    Ok(states)
}

pub(super) async fn latest_component_activation_state(
    transaction: &impl QueryExecutor,
    component: &str,
) -> ConfigurationStoreResult<Option<StoredComponentActivationState>> {
    let mut rows = transaction
        .query(
            "SELECT component, desired_revision_id, observed_revision_id,
                    last_working_revision_id, restart_required, activation_error_code
             FROM configuration_component_activation_events
             WHERE component = ?1
             ORDER BY event_id DESC
             LIMIT 1",
            params![component],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let state = decode_component_activation_state(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration component latest activation resolved to multiple rows",
        ));
    }
    Ok(Some(state))
}

pub(super) async fn insert_component_activation_event(
    transaction: &impl Executor,
    state: &StoredComponentActivationState,
    occurred_at: UtcMicros,
) -> ConfigurationStoreResult<()> {
    validate_component_name(&state.component).map_err(invalid_store_data)?;
    validate_activation_error_code(state.activation_error_code.as_deref())
        .map_err(invalid_store_data)?;
    transaction
        .execute(
            "INSERT INTO configuration_component_activation_events (
                component, desired_revision_id, observed_revision_id,
                last_working_revision_id, restart_required, activation_error_code, occurred_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                state.component.clone(),
                state.desired_revision_id.as_str(),
                state
                    .observed_revision_id
                    .as_ref()
                    .map(|value| value.as_str().to_owned()),
                state
                    .last_working_revision_id
                    .as_ref()
                    .map(|value| value.as_str().to_owned()),
                i64::from(u8::from(state.restart_required)),
                state.activation_error_code.clone(),
                occurred_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

pub(super) async fn advance_component_desired_state(
    transaction: &impl Executor,
    desired_revision_id: &ConfigurationRevisionId,
    occurred_at: UtcMicros,
) -> ConfigurationStoreResult<()> {
    // The shared statement copies the latest rows; decoding them first keeps a
    // malformed stored row from propagating into the event it appends.
    latest_component_activation_states(transaction).await?;
    transaction
        .execute(
            ADVANCE_COMPONENT_DESIRED_STATE_SQL,
            params![desired_revision_id.as_str(), occurred_at.0],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

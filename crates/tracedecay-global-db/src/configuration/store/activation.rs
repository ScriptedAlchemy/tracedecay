//! Desired/observed component activation state persistence.

use super::codec::{decode_id, unavailable_store};
use super::{
    ConfigurationRevisionId, ConfigurationStoreResult, Executor, QueryExecutor, Row, UtcMicros,
    invalid_store_data, params,
};

#[derive(Clone, Debug)]
pub(super) struct StoredComponentActivationState {
    pub(super) component: String,
    pub(super) desired_revision_id: ConfigurationRevisionId,
    pub(super) observed_revision_id: Option<ConfigurationRevisionId>,
    pub(super) last_working_revision_id: Option<ConfigurationRevisionId>,
    pub(super) restart_required: bool,
    pub(super) activation_error_code: Option<String>,
}

pub(super) fn validate_component_name(component: &str) -> ConfigurationStoreResult<()> {
    if component.is_empty()
        || component.trim() != component
        || component.len() > 256
        || component.chars().any(char::is_control)
    {
        return Err(invalid_store_data(
            "configuration component name is not canonical",
        ));
    }
    Ok(())
}

pub(super) fn validate_activation_error_code(code: Option<&str>) -> ConfigurationStoreResult<()> {
    let Some(code) = code else {
        return Ok(());
    };
    if code.is_empty()
        || code.trim() != code
        || code.len() > 256
        || code.chars().any(char::is_control)
    {
        return Err(invalid_store_data(
            "configuration activation error code is not canonical",
        ));
    }
    Ok(())
}

pub(super) fn decode_component_activation_state(
    row: &Row,
) -> ConfigurationStoreResult<StoredComponentActivationState> {
    let component = row
        .get::<String>(0)
        .map_err(|error| invalid_store_data(format!("read configuration component: {error}")))?;
    validate_component_name(&component)?;
    let desired_revision_id = decode_id(
        row.get::<String>(1).map_err(|error| {
            invalid_store_data(format!("read desired configuration revision: {error}"))
        })?,
        "desired component revision id",
    )?;
    let observed_revision_id = row
        .get::<Option<String>>(2)
        .map_err(|error| {
            invalid_store_data(format!("read observed configuration revision: {error}"))
        })?
        .map(|value| decode_id(value, "observed component revision id"))
        .transpose()?;
    let last_working_revision_id = row
        .get::<Option<String>>(3)
        .map_err(|error| {
            invalid_store_data(format!("read last working configuration revision: {error}"))
        })?
        .map(|value| decode_id(value, "last working component revision id"))
        .transpose()?;
    let restart_required = match row
        .get::<i64>(4)
        .map_err(|error| invalid_store_data(format!("read configuration restart state: {error}")))?
    {
        0 => false,
        1 => true,
        _ => {
            return Err(invalid_store_data(
                "stored configuration restart state is invalid",
            ));
        }
    };
    let activation_error_code = row
        .get::<Option<String>>(5)
        .map_err(|error| invalid_store_data(format!("read activation error code: {error}")))?;
    validate_activation_error_code(activation_error_code.as_deref())?;
    Ok(StoredComponentActivationState {
        component,
        desired_revision_id,
        observed_revision_id,
        last_working_revision_id,
        restart_required,
        activation_error_code,
    })
}

pub(super) async fn latest_component_activation_states(
    transaction: &impl QueryExecutor,
) -> ConfigurationStoreResult<Vec<StoredComponentActivationState>> {
    let mut rows = transaction
        .query(
            "SELECT event.component, event.desired_revision_id, event.observed_revision_id,
                    event.last_working_revision_id, event.restart_required,
                    event.activation_error_code
             FROM configuration_component_activation_events AS event
             WHERE event.event_id = (
                 SELECT MAX(candidate.event_id)
                 FROM configuration_component_activation_events AS candidate
                 WHERE candidate.component = event.component
             )
             ORDER BY event.component ASC",
            (),
        )
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
    validate_component_name(&state.component)?;
    validate_activation_error_code(state.activation_error_code.as_deref())?;
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
    for prior in latest_component_activation_states(transaction).await? {
        let observed_revision_id = prior
            .observed_revision_id
            .clone()
            .or_else(|| prior.last_working_revision_id.clone());
        let restart_required = observed_revision_id.as_ref() != Some(desired_revision_id)
            || prior.activation_error_code.is_some();
        insert_component_activation_event(
            transaction,
            &StoredComponentActivationState {
                component: prior.component,
                desired_revision_id: desired_revision_id.clone(),
                observed_revision_id,
                last_working_revision_id: prior.last_working_revision_id,
                restart_required,
                activation_error_code: prior.activation_error_code,
            },
            occurred_at,
        )
        .await?;
    }
    Ok(())
}

//! One receipt and desired-state write for both configuration writers.
//!
//! The repository executor and the global-db control store commit the same
//! tables through different drivers. The statements live here so those drivers
//! cannot drift on `activation_status` or on which component rows a desired
//! revision advances.

use rusqlite::{Connection, params};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationLayerIdV1,
    ConfigurationRevisionId,
};
use tracedecay_store::ConfigurationCommitV1;

use super::support::invalid;

pub const CONFIGURATION_ACTIVATION_DESIRED_RECORDED: &str = "desired_recorded_v1";

pub const INSERT_CONFIGURATION_MUTATION_RECEIPT_SQL: &str = "
INSERT INTO configuration_mutation_receipts (
    receipt_id, plan_id, actor_id, idempotency_key,
    base_revision_id, result_revision_id, operation_digest,
    authorization_policy_epoch, authorization_policy_digest,
    authority_revalidated_at, activation_status, receipt_digest, created_at,
    effective_deadline_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
";

/// Appends one activation event per component whose latest row already exists.
///
/// Observed state is the stored observation, or the last working revision when
/// observation was never recorded. Restart stays clear only when that observed
/// revision is the new desired revision and the prior row has no error.
/// Components with no row are left alone. Insert order follows component name
/// so later event ids stay comparable with the previous per-component loop.
pub const ADVANCE_COMPONENT_DESIRED_STATE_SQL: &str = "
INSERT INTO configuration_component_activation_events (
    component, desired_revision_id, observed_revision_id,
    last_working_revision_id, restart_required, activation_error_code, occurred_at
)
SELECT
    event.component,
    ?1,
    COALESCE(event.observed_revision_id, event.last_working_revision_id),
    event.last_working_revision_id,
    CASE
        WHEN COALESCE(event.observed_revision_id, event.last_working_revision_id) = ?1
             AND event.activation_error_code IS NULL
        THEN 0
        ELSE 1
    END,
    event.activation_error_code,
    ?2
FROM configuration_component_activation_events AS event
WHERE event.event_id = (
    SELECT MAX(candidate.event_id)
    FROM configuration_component_activation_events AS candidate
    WHERE candidate.component = event.component
)
ORDER BY event.component ASC
";

/// Layer columns copied from an already-typed candidate.
///
/// The default layer is the empty id. `configuration_entries.layer_id` is part
/// of the primary key, and a NULL there is not the same stored layer.
pub fn configuration_entry_layer(
    provenance: &[ConfigurationCandidateV1],
) -> (&'static str, String) {
    let layer = provenance
        .iter()
        .find(|candidate| {
            matches!(
                candidate.disposition,
                CandidateDispositionV1::Winning | CandidateDispositionV1::Defaulted
            )
        })
        .or_else(|| provenance.first())
        .map(|candidate| &candidate.layer);
    match layer {
        Some(ConfigurationLayerIdV1::UserProfile { profile_id }) => {
            ("user_profile", profile_id.as_str().to_owned())
        }
        Some(ConfigurationLayerIdV1::Project { project_id }) => {
            ("project", project_id.as_str().to_owned())
        }
        Some(ConfigurationLayerIdV1::Collection { collection_id }) => {
            ("collection", collection_id.as_str().to_owned())
        }
        Some(ConfigurationLayerIdV1::Default) | None => ("default", String::new()),
    }
}

pub fn insert_mutation_receipt(
    connection: &Connection,
    commit: &ConfigurationCommitV1,
) -> rusqlite::Result<()> {
    let authorization_policy_epoch = i64::try_from(commit.receipt.authorization_policy_epoch)
        .map_err(|_| invalid("configuration policy epoch exceeds i64"))?;
    let plan_id = commit
        .change_plan
        .as_ref()
        .map(|plan| plan.plan_id.as_str());
    connection.execute(
        INSERT_CONFIGURATION_MUTATION_RECEIPT_SQL,
        params![
            commit.receipt.receipt_id.as_str(),
            plan_id,
            commit.receipt.actor_id.as_str(),
            commit.receipt.idempotency_key.as_str(),
            commit.receipt.base_revision_id.as_str(),
            commit.receipt.result_revision_id.as_str(),
            commit.receipt.operation_digest.as_str(),
            authorization_policy_epoch,
            commit.receipt.authorization_policy_digest.as_str(),
            commit.receipt.authority_revalidated_at.0,
            CONFIGURATION_ACTIVATION_DESIRED_RECORDED,
            commit.receipt.receipt_digest.as_str(),
            commit.receipt.created_at.0,
            commit.receipt.effective_deadline_at.0,
        ],
    )?;
    Ok(())
}

pub fn advance_component_desired_state(
    connection: &Connection,
    revision_id: &ConfigurationRevisionId,
    occurred_at: UtcMicros,
) -> rusqlite::Result<()> {
    connection.execute(
        ADVANCE_COMPONENT_DESIRED_STATE_SQL,
        params![revision_id.as_str(), occurred_at.0],
    )?;
    Ok(())
}

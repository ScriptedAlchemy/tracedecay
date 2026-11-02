use serde::Serialize;
use serde_json::{Value, json};

use crate::automation::automatic_facts::AdmittedAutomaticFact;
use tracedecay_domain::errors::{Result, TraceDecayError};

fn values_digest<T: Serialize>(values: &[T]) -> Result<String> {
    // Summaries retain the report JSON's ordering even when their source
    // collection carries typed commands rather than already-serialized values.
    let values = serde_json::to_value(values).map_err(TraceDecayError::from)?;
    let bytes = serde_json::to_vec(&values).map_err(TraceDecayError::from)?;
    Ok(crate::automation::artifact_refs::sha256_bytes(&bytes))
}

pub(super) fn fact_collection_summary<T: Serialize>(values: &[T]) -> Result<Value> {
    Ok(json!({"count": values.len(), "sha256": values_digest(values)?}))
}

pub(super) fn session_fact_finalization_failure_summary(proposed: &[Value]) -> Result<Value> {
    Ok(json!({"schema_version": 1, "proposed": fact_collection_summary(proposed)?}))
}

pub(super) fn session_fact_ledger_summary(
    proposed: &[Value],
    accepted: &[AdmittedAutomaticFact],
    admitted: &[AdmittedAutomaticFact],
    quarantined: &[Value],
) -> Result<Value> {
    Ok(json!({
        "schema_version": 1,
        "proposed": fact_collection_summary(proposed)?,
        "accepted": fact_collection_summary(accepted)?,
        "admitted": fact_collection_summary(admitted)?,
        "quarantined": fact_collection_summary(quarantined)?,
    }))
}

pub(super) fn validation_repairs_summary(repairs: &[Value]) -> Result<Value> {
    Ok(json!({"count": repairs.len(), "sha256": values_digest(repairs)?}))
}

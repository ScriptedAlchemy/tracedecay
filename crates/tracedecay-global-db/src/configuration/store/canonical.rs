//! Exact final-snapshot validation.

use std::collections::BTreeSet;

use super::{ConfigurationError, ConfigurationRegistry, ConfigurationSnapshotV1};

pub(super) fn validate_final_snapshot(
    snapshot: &ConfigurationSnapshotV1,
) -> Result<(), ConfigurationError> {
    let registry = ConfigurationRegistry::core().map_err(ConfigurationError::validation)?;
    let expected = registry
        .definitions()
        .map(|definition| definition.key.clone())
        .collect::<BTreeSet<_>>();
    let actual = snapshot
        .effective_values
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(ConfigurationError::ResetRequired);
    }
    for (key, value) in &snapshot.effective_values {
        registry
            .validate_value(key, value)
            .map_err(|_| ConfigurationError::ResetRequired)?;
    }
    Ok(())
}

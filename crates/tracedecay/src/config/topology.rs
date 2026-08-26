//! Control-plane helpers for the single complete topology-policy setting.
//!
//! The sole resolver combines the typed registry default with explicit layers;
//! these helpers consume its pinned snapshot and return the complete validated
//! policy. They never inspect paths, invoke Git, manufacture capability or
//! repository evidence, or substitute a locally invented default — missing,
//! mistyped, invalid, or unsupported inputs fail closed.

use thiserror::Error;
use tracedecay_domain::DomainError;
use tracedecay_domain::configuration::{
    ConfigurationSnapshotV1, ConfigurationValueV1, SettingKey, WORK_TOPOLOGY_POLICY_SETTING_KEY,
    WorkTopologyPolicyV1, safe_work_topology_policy_v1,
};

#[derive(Debug, Error)]
pub enum TopologyConfigurationError {
    #[error("topology setting key is invalid: {0}")]
    Domain(#[from] DomainError),
    #[error("topology policy is absent from the resolved configuration snapshot")]
    MissingTopologyPolicy,
    #[error("resolved topology setting has an unexpected typed value")]
    WrongTopologyValue,
}

/// Resolve the one complete policy from a pinned configuration snapshot. No
/// adapter defaults are consulted if the setting is missing or malformed.
pub fn resolved_work_topology_policy(
    snapshot: &ConfigurationSnapshotV1,
) -> Result<&WorkTopologyPolicyV1, TopologyConfigurationError> {
    snapshot.validate()?;
    let key = SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY)?;
    match snapshot.effective_values.get(&key) {
        Some(ConfigurationValueV1::WorkTopologyPolicy(policy)) => {
            policy.validate()?;
            Ok(policy)
        }
        Some(_) => Err(TopologyConfigurationError::WrongTopologyValue),
        None => Err(TopologyConfigurationError::MissingTopologyPolicy),
    }
}

/// Exposes the exact safe policy used by the typed registry before any
/// operator publishes a protected replacement.
pub fn safe_default_work_topology_policy() -> WorkTopologyPolicyV1 {
    safe_work_topology_policy_v1()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_default_never_enables_cross_merge() {
        let policy = safe_default_work_topology_policy();
        policy.validate().unwrap();
        assert!(!policy.cross_merge.allow_cross_repository);
    }
}

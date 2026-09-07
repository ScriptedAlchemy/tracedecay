//! Control-plane resolution of the single analyzer-settings configuration key.
//!
//! `analyzer.settings.v1` is a registered, project-scoped, operator-settable
//! key. The sole resolver combines its typed registry default with explicit
//! layers; these helpers consume that pinned snapshot and return the complete
//! validated selection set. They never probe an executable, read host state,
//! consult an adapter default, or substitute an empty selection for a value
//! they could not read — a missing, mistyped, tampered, or invalid input fails
//! closed with a typed error.

use thiserror::Error;
use tracedecay_domain::DomainError;
use tracedecay_domain::configuration::{
    ANALYZER_SETTINGS_SETTING_KEY, AnalyzerLanguageId, AnalyzerLanguageSelectionV1,
    AnalyzerSettingsV1, ConfigurationSnapshotV1, ConfigurationValueV1, SettingKey,
};

#[derive(Debug, Error)]
pub enum AnalyzerConfigurationError {
    #[error("analyzer setting key or stored value is invalid: {0}")]
    Domain(#[from] DomainError),
    #[error("analyzer settings are absent from the resolved configuration snapshot")]
    MissingAnalyzerSettings,
    #[error("resolved analyzer setting has an unexpected typed value")]
    WrongAnalyzerValue,
}

/// Resolve the one complete analyzer selection set from a pinned configuration
/// snapshot. No adapter default is consulted if the setting is missing or
/// malformed; an unset key still resolves because the sole resolver merges the
/// registered default into the snapshot's effective values.
pub fn resolved_analyzer_settings(
    snapshot: &ConfigurationSnapshotV1,
) -> Result<&AnalyzerSettingsV1, AnalyzerConfigurationError> {
    snapshot.validate()?;
    let key = SettingKey::new(ANALYZER_SETTINGS_SETTING_KEY)?;
    match snapshot.effective_values.get(&key) {
        Some(ConfigurationValueV1::AnalyzerSettings(settings)) => {
            settings.validate()?;
            Ok(settings)
        }
        Some(_) => Err(AnalyzerConfigurationError::WrongAnalyzerValue),
        None => Err(AnalyzerConfigurationError::MissingAnalyzerSettings),
    }
}

/// Exposes the exact selection set the typed registry uses before any operator
/// publishes a replacement: no analyzer is configured for any language.
pub fn default_analyzer_settings() -> AnalyzerSettingsV1 {
    AnalyzerSettingsV1::empty()
}

/// The one operator-published selection for `language`, if the resolved
/// settings carry it. `AnalyzerSettingsV1::validate` already rejects duplicate
/// or unordered language ids, so at most one selection can match.
pub fn configured_language_selection<'a>(
    settings: &'a AnalyzerSettingsV1,
    language: &AnalyzerLanguageId,
) -> Option<&'a AnalyzerLanguageSelectionV1> {
    settings
        .selections
        .iter()
        .find(|selection| &selection.language_id == language)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use tracedecay_domain::configuration::{
        AnalyzerExecutableId, AnalyzerExecutableReferenceV1, AnalyzerPrivacyClassV1,
        AnalyzerResourceLimitsV1, AnalyzerRestartPolicyV1, ConfigurationLayerIdV1,
        ConfigurationValueKindV1, RestartRequirementV1, SettingScopeV1, SettingSensitivityV1,
    };
    use tracedecay_domain::{ManifestDigest, ProjectId, UserProfileId};
    use tracedecay_global_db::configuration::registry::ConfigurationRegistry;
    use tracedecay_global_db::configuration::resolver::{
        ConfigurationLayerV1, resolve_configuration,
    };

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture id is canonical")
    }

    fn analyzer_key() -> SettingKey {
        SettingKey::new(ANALYZER_SETTINGS_SETTING_KEY).unwrap()
    }

    fn selection(language: &str, enabled: bool) -> AnalyzerLanguageSelectionV1 {
        AnalyzerLanguageSelectionV1 {
            language_id: AnalyzerLanguageId::new(language.to_owned()).unwrap(),
            enabled,
            executable: AnalyzerExecutableReferenceV1::BuiltIn {
                executable_id: AnalyzerExecutableId::new(format!("analyzer.{language}.builtin"))
                    .unwrap(),
            },
            arguments: Vec::new(),
            initialization_options: BTreeMap::new(),
            settings: BTreeMap::new(),
            environment_allowlist: BTreeSet::new(),
            privacy_class: AnalyzerPrivacyClassV1::NonSensitive,
            resource_limits: AnalyzerResourceLimitsV1 {
                maximum_memory_mib: 512,
                startup_timeout_millis: 5_000,
                request_timeout_millis: 5_000,
            },
            restart_policy: AnalyzerRestartPolicyV1::RestartOnConfigurationChange,
        }
    }

    fn settings(selections: Vec<AnalyzerLanguageSelectionV1>) -> AnalyzerSettingsV1 {
        AnalyzerSettingsV1 {
            schema_version: AnalyzerSettingsV1::SCHEMA_VERSION,
            selections,
        }
    }

    fn project_layer(value: AnalyzerSettingsV1) -> ConfigurationLayerV1 {
        ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id::<ProjectId>("project.fixture"),
            },
            revision_id: id("revision.project.1"),
            entries: BTreeMap::from([(
                analyzer_key(),
                ConfigurationValueV1::AnalyzerSettings(value),
            )]),
        }
    }

    #[test]
    fn registry_default_is_the_empty_selection_set() {
        let registry = ConfigurationRegistry::core().unwrap();
        let definition = registry.definition(&analyzer_key()).unwrap();
        assert_eq!(
            definition.value_kind,
            ConfigurationValueKindV1::AnalyzerSettings
        );
        assert_eq!(definition.sensitivity, SettingSensitivityV1::Sensitive);
        assert_eq!(definition.scope, SettingScopeV1::Project);
        assert_eq!(
            definition.restart_requirement,
            RestartRequirementV1::AnalyzerRestart
        );
        let ConfigurationValueV1::AnalyzerSettings(default) = &definition.default_value else {
            panic!("registry default must be a typed analyzer settings value");
        };
        assert_eq!(*default, default_analyzer_settings());
    }

    #[test]
    fn unset_key_resolves_to_the_registered_default() {
        let registry = ConfigurationRegistry::core().unwrap();
        let snapshot = resolve_configuration(&registry, &[]).unwrap().snapshot;
        let resolved = resolved_analyzer_settings(&snapshot).unwrap();
        let default = default_analyzer_settings();
        assert_eq!(*resolved, default);
        assert!(resolved.selections.is_empty());
        assert_eq!(
            resolved.compute_digest().unwrap(),
            default.compute_digest().unwrap()
        );
    }

    #[test]
    fn stored_project_value_resolves_into_the_typed_accessor() {
        let registry = ConfigurationRegistry::core().unwrap();
        let published = settings(vec![selection("python", false), selection("rust", true)]);
        published.validate().unwrap();

        let resolution =
            resolve_configuration(&registry, &[project_layer(published.clone())]).unwrap();
        let resolved = resolved_analyzer_settings(&resolution.snapshot).unwrap();
        assert_eq!(*resolved, published);
        assert_ne!(*resolved, default_analyzer_settings());
        assert_eq!(
            resolved.compute_digest().unwrap(),
            published.compute_digest().unwrap()
        );

        // The published selection is reachable per language, including the
        // explicitly disabled one — the accessor never hides a row.
        let rust = configured_language_selection(
            resolved,
            &AnalyzerLanguageId::new("rust".to_owned()).unwrap(),
        )
        .expect("published rust selection");
        assert!(rust.enabled);
        let python = configured_language_selection(
            resolved,
            &AnalyzerLanguageId::new("python".to_owned()).unwrap(),
        )
        .expect("published python selection");
        assert!(!python.enabled);
        assert!(
            configured_language_selection(
                resolved,
                &AnalyzerLanguageId::new("typescript".to_owned()).unwrap(),
            )
            .is_none()
        );

        // The behavior digest moves with the published selection even though
        // the resolution path is identical.
        let baseline = resolve_configuration(&registry, &[]).unwrap();
        assert_ne!(
            baseline.snapshot.effective_behavior_digest,
            resolution.snapshot.effective_behavior_digest
        );
    }

    #[test]
    fn user_profile_layer_cannot_override_project_scoped_analyzer_settings() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::UserProfile {
                profile_id: id::<UserProfileId>("profile.fixture"),
            },
            revision_id: id("revision.profile.1"),
            entries: BTreeMap::from([(
                analyzer_key(),
                ConfigurationValueV1::AnalyzerSettings(settings(vec![selection("rust", true)])),
            )]),
        };
        assert!(resolve_configuration(&registry, &[layer]).is_err());
    }

    #[test]
    fn malformed_stored_value_in_a_layer_fails_closed() {
        let registry = ConfigurationRegistry::core().unwrap();

        // Unsupported schema version.
        let mut future = settings(vec![selection("rust", true)]);
        future.schema_version = 2;
        assert!(resolve_configuration(&registry, &[project_layer(future)]).is_err());

        // Non-canonical selection order (also catches duplicate languages).
        let unordered = settings(vec![selection("rust", true), selection("python", true)]);
        assert!(resolve_configuration(&registry, &[project_layer(unordered)]).is_err());

        // Invalid resource limits inside an otherwise well-formed selection.
        let mut zero_memory = selection("rust", true);
        zero_memory.resource_limits.maximum_memory_mib = 0;
        assert!(
            resolve_configuration(&registry, &[project_layer(settings(vec![zero_memory]))])
                .is_err()
        );

        // A mistyped value at the analyzer key never reaches a snapshot.
        let mistyped = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id::<ProjectId>("project.fixture"),
            },
            revision_id: id("revision.project.1"),
            entries: BTreeMap::from([(
                analyzer_key(),
                ConfigurationValueV1::Text("disabled".to_owned()),
            )]),
        };
        assert!(resolve_configuration(&registry, &[mistyped]).is_err());
    }

    #[test]
    fn snapshot_resolution_requires_the_typed_analyzer_value() {
        // Missing key fails closed rather than inventing an empty selection set.
        let empty = ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new()).unwrap();
        assert!(matches!(
            resolved_analyzer_settings(&empty),
            Err(AnalyzerConfigurationError::MissingAnalyzerSettings)
        ));

        // A mistyped value at the analyzer key fails closed.
        let registry = ConfigurationRegistry::core().unwrap();
        let default_candidate = resolve_configuration(&registry, &[])
            .unwrap()
            .settings
            .get(&analyzer_key())
            .unwrap()
            .candidates
            .first()
            .unwrap()
            .clone();
        let mistyped = ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                analyzer_key(),
                ConfigurationValueV1::Text("disabled".to_owned()),
            )]),
            BTreeMap::from([(analyzer_key(), vec![default_candidate])]),
        )
        .unwrap();
        assert!(matches!(
            resolved_analyzer_settings(&mistyped),
            Err(AnalyzerConfigurationError::WrongAnalyzerValue)
        ));

        // A malformed typed value substituted into an otherwise pinned snapshot
        // fails closed on the snapshot identity, before the value is read.
        let published = settings(vec![selection("rust", true)]);
        let mut tampered = resolve_configuration(&registry, &[project_layer(published)])
            .unwrap()
            .snapshot;
        let mut malformed = settings(vec![selection("rust", true)]);
        malformed.schema_version = 2;
        tampered.effective_values.insert(
            analyzer_key(),
            ConfigurationValueV1::AnalyzerSettings(malformed),
        );
        assert!(matches!(
            resolved_analyzer_settings(&tampered),
            Err(AnalyzerConfigurationError::Domain(_))
        ));

        // A tampered snapshot identity fails closed even with a valid value.
        let mut snapshot = resolve_configuration(&registry, &[]).unwrap().snapshot;
        snapshot.effective_behavior_digest =
            ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
        assert!(matches!(
            resolved_analyzer_settings(&snapshot),
            Err(AnalyzerConfigurationError::Domain(_))
        ));
    }
}

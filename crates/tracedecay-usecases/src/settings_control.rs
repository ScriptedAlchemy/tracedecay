//! Canonical typed preparation for dashboard-visible settings mutations.
//!
//! HTTP parses the transport body and renders errors. Validation, target
//! selection, typed mutation construction, and restart/resync semantics live
//! here before the shared configuration application handler performs
//! authorization, CAS, receipt issuance, status, and rollback.

use serde::Deserialize;
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationSnapshotV1, ConfigurationValueV1, ContextScoutConfigurationStateV1,
    ContextScoutSettingsV1, INDEX_EXCLUDE_SETTING_KEY, INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY,
    INDEX_GIT_IGNORE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY, INDEX_MAX_FILE_SIZE_SETTING_KEY,
    INDEX_TRACK_CALL_SITES_SETTING_KEY, SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY, SettingKey, TELEMETRY_TIMINGS_SETTING_KEY,
};

use tracedecay_application::{ProjectSettingsPatchInputV1, validate_project_settings_patch};

use crate::config::PinnedRuntimeConfiguration;
use crate::configuration::DirectConfigurationMutation;

pub use tracedecay_application::SettingsValidationIssueV1;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectSettingsPatchV1 {
    pub expected_revision_id: String,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
    #[serde(default)]
    pub extract_docstrings: Option<bool>,
    #[serde(default)]
    pub track_call_sites: Option<bool>,
    #[serde(default)]
    pub git_ignore: Option<bool>,
    #[serde(default)]
    pub telemetry: Option<TelemetrySettingsPatchV1>,
    #[serde(default)]
    pub sync: Option<SyncSettingsPatchV1>,
    #[serde(default)]
    pub context_scout: Option<bool>,
}

/// The effective Plan 20 Context Scout value in one configuration snapshot.
/// The registry always resolves this key; a snapshot predating the key reads
/// as the canonical stock state, which is disabled.
pub fn effective_context_scout_settings(
    snapshot: &ConfigurationSnapshotV1,
) -> ContextScoutSettingsV1 {
    SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY)
        .ok()
        .and_then(|key| match snapshot.effective_values.get(&key) {
            Some(ConfigurationValueV1::ContextScoutSettings(settings)) => Some(settings.clone()),
            _ => None,
        })
        .unwrap_or_else(ContextScoutSettingsV1::disabled)
}

pub fn context_scout_settings_are_enabled(settings: &ContextScoutSettingsV1) -> bool {
    settings.state != ContextScoutConfigurationStateV1::Disabled
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SyncSettingsPatchV1 {
    #[serde(default)]
    pub auto_track_pr_branches: Option<bool>,
    #[serde(default)]
    pub auto_track_pr_poll_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySettingsPatchV1 {
    #[serde(default)]
    pub timings: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ProjectSettingsPreviewV1 {
    pub expected_revision: ConfigurationRevisionId,
    pub mutation: DirectConfigurationMutation,
    pub changed: bool,
    pub resync_recommended: bool,
}

#[derive(Clone, Debug)]
pub enum ProjectSettingsPreviewErrorV1 {
    Validation(Vec<SettingsValidationIssueV1>),
    RevisionConflict { expected: String, actual: String },
    InvalidAuthority,
}

pub fn preview_project_settings(
    project_id: &ProjectId,
    current: &PinnedRuntimeConfiguration,
    patch: ProjectSettingsPatchV1,
) -> Result<ProjectSettingsPreviewV1, ProjectSettingsPreviewErrorV1> {
    if &current.target.project_id != project_id {
        return Err(ProjectSettingsPreviewErrorV1::InvalidAuthority);
    }
    let expected_revision = ConfigurationRevisionId::new(patch.expected_revision_id.clone())
        .map_err(|_| ProjectSettingsPreviewErrorV1::InvalidAuthority)?;
    if let Err(issues) = validate_project_settings_patch(&ProjectSettingsPatchInputV1 {
        include: patch.include.clone(),
        exclude: patch.exclude.clone(),
        max_file_size: patch.max_file_size,
        auto_track_pr_poll_secs: patch
            .sync
            .as_ref()
            .and_then(|sync| sync.auto_track_pr_poll_secs),
    }) {
        return Err(ProjectSettingsPreviewErrorV1::Validation(issues));
    }
    let mut issues = Vec::new();
    if let Some(globs) = &patch.include {
        validate_globs("include", globs, &mut issues);
    }
    if let Some(globs) = &patch.exclude {
        validate_globs("exclude", globs, &mut issues);
    }
    if !issues.is_empty() {
        return Err(ProjectSettingsPreviewErrorV1::Validation(issues));
    }

    let layer = ConfigurationLayerIdV1::Project {
        project_id: project_id.clone(),
    };
    let current_context_scout = effective_context_scout_settings(&current.snapshot);
    let expected_is_current = expected_revision == current.revision_id;
    let supplied_values_are_current = patch
        .include
        .as_ref()
        .is_none_or(|value| value == &current.config.include)
        && patch
            .exclude
            .as_ref()
            .is_none_or(|value| value == &current.config.exclude)
        && patch
            .max_file_size
            .is_none_or(|value| value == current.config.max_file_size)
        && patch
            .extract_docstrings
            .is_none_or(|value| value == current.config.extract_docstrings)
        && patch
            .track_call_sites
            .is_none_or(|value| value == current.config.track_call_sites)
        && patch
            .git_ignore
            .is_none_or(|value| value == current.config.git_ignore)
        && patch.telemetry.as_ref().is_none_or(|telemetry| {
            telemetry
                .timings
                .is_none_or(|value| value == current.config.telemetry.timings)
        })
        && patch.sync.as_ref().is_none_or(|sync| {
            sync.auto_track_pr_branches
                .is_none_or(|value| value == current.config.sync.auto_track_pr_branches)
                && sync
                    .auto_track_pr_poll_secs
                    .is_none_or(|value| value == current.config.sync.auto_track_pr_poll_secs)
        })
        && patch.context_scout.is_none_or(|value| {
            value == context_scout_settings_are_enabled(&current_context_scout)
        });
    let mut mutations = Vec::new();
    push(
        &mut mutations,
        &layer,
        INDEX_INCLUDE_SETTING_KEY,
        patch.include.map(ConfigurationValueV1::StringList),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_EXCLUDE_SETTING_KEY,
        patch.exclude.map(ConfigurationValueV1::StringList),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_MAX_FILE_SIZE_SETTING_KEY,
        patch.max_file_size.map(ConfigurationValueV1::Unsigned),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY,
        patch.extract_docstrings.map(ConfigurationValueV1::Boolean),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_TRACK_CALL_SITES_SETTING_KEY,
        patch.track_call_sites.map(ConfigurationValueV1::Boolean),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_GIT_IGNORE_SETTING_KEY,
        patch.git_ignore.map(ConfigurationValueV1::Boolean),
    )?;
    if let Some(telemetry) = patch.telemetry {
        push(
            &mut mutations,
            &layer,
            TELEMETRY_TIMINGS_SETTING_KEY,
            telemetry.timings.map(ConfigurationValueV1::Boolean),
        )?;
    }
    if let Some(sync) = patch.sync {
        push(
            &mut mutations,
            &layer,
            SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
            sync.auto_track_pr_branches
                .map(ConfigurationValueV1::Boolean),
        )?;
        push(
            &mut mutations,
            &layer,
            SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
            sync.auto_track_pr_poll_secs
                .map(ConfigurationValueV1::Unsigned),
        )?;
    }
    // The dashboard flag is only a state toggle: mode, limits, and model
    // selection stay exactly as configured, and disabling never erases them.
    push(
        &mut mutations,
        &layer,
        CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
        patch.context_scout.map(|enabled| {
            let mut settings = current_context_scout.clone();
            settings.state = if enabled {
                ContextScoutConfigurationStateV1::Active
            } else {
                ContextScoutConfigurationStateV1::Disabled
            };
            ConfigurationValueV1::ContextScoutSettings(settings)
        }),
    )?;
    if mutations.is_empty() && !expected_is_current {
        return Err(ProjectSettingsPreviewErrorV1::RevisionConflict {
            expected: expected_revision.as_str().to_owned(),
            actual: current.revision_id.as_str().to_owned(),
        });
    }
    let changed = !expected_is_current || !supplied_values_are_current;
    Ok(ProjectSettingsPreviewV1 {
        expected_revision,
        mutation: DirectConfigurationMutation::Batch { mutations },
        changed,
        resync_recommended: changed,
    })
}

fn push(
    mutations: &mut Vec<DirectConfigurationMutation>,
    layer: &ConfigurationLayerIdV1,
    key: &str,
    value: Option<ConfigurationValueV1>,
) -> Result<(), ProjectSettingsPreviewErrorV1> {
    let Some(value) = value else {
        return Ok(());
    };
    let key = SettingKey::new(key).map_err(|_| ProjectSettingsPreviewErrorV1::InvalidAuthority)?;
    mutations.push(DirectConfigurationMutation::Set {
        layer: layer.clone(),
        key,
        value: Box::new(value),
    });
    Ok(())
}

fn validate_globs(field: &str, globs: &[String], issues: &mut Vec<SettingsValidationIssueV1>) {
    for pattern in globs {
        if pattern.trim().is_empty() {
            issues.push(issue(field, &format!("{field} patterns must not be empty")));
        } else if let Err(error) = glob::Pattern::new(pattern) {
            issues.push(issue(
                field,
                &format!("invalid glob pattern '{pattern}': {error}"),
            ));
        }
    }
}

fn issue(field: &str, message: &str) -> SettingsValidationIssueV1 {
    SettingsValidationIssueV1 {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use tracedecay_domain::configuration::{ConfigurationCandidateV1, ConfigurationSnapshotV1};

    #[test]
    fn preview_builds_one_typed_atomic_batch_without_mutating_runtime() {
        let project_id = ProjectId::new("project.settings").unwrap();
        let revision = ConfigurationRevisionId::new("configuration.revision.settings").unwrap();
        let snapshot = ConfigurationSnapshotV1::new(
            BTreeMap::new(),
            BTreeMap::<SettingKey, Vec<ConfigurationCandidateV1>>::new(),
        )
        .unwrap();
        let current = PinnedRuntimeConfiguration {
            target: crate::config::RuntimeConfigurationTarget {
                project_id: project_id.clone(),
                project_root: PathBuf::from("/project"),
            },
            revision_id: revision.clone(),
            snapshot,
            config: crate::config::TraceDecayConfig::default(),
        };
        let changed_timings = !current.config.telemetry.timings;
        let preview = preview_project_settings(
            &project_id,
            &current,
            ProjectSettingsPatchV1 {
                expected_revision_id: revision.as_str().to_owned(),
                max_file_size: Some(42),
                telemetry: Some(TelemetrySettingsPatchV1 {
                    timings: Some(changed_timings),
                }),
                ..ProjectSettingsPatchV1::default()
            },
        )
        .unwrap();
        let DirectConfigurationMutation::Batch { mutations } = preview.mutation else {
            panic!("project settings must be atomic")
        };
        assert_eq!(mutations.len(), 2);
        assert_eq!(current.config.max_file_size, 1_048_576);
    }

    #[test]
    fn context_scout_flag_toggles_only_the_state_of_the_effective_value() {
        let project_id = ProjectId::new("project.settings.scout").unwrap();
        let revision = ConfigurationRevisionId::new("configuration.revision.scout").unwrap();
        let snapshot = ConfigurationSnapshotV1::new(
            BTreeMap::new(),
            BTreeMap::<SettingKey, Vec<ConfigurationCandidateV1>>::new(),
        )
        .unwrap();
        let current = PinnedRuntimeConfiguration {
            target: crate::config::RuntimeConfigurationTarget {
                project_id: project_id.clone(),
                project_root: PathBuf::from("/project"),
            },
            revision_id: revision.clone(),
            snapshot,
            config: crate::config::TraceDecayConfig::default(),
        };
        // A snapshot without the key renders the canonical stock state: off.
        let current_settings = effective_context_scout_settings(&current.snapshot);
        assert_eq!(current_settings, ContextScoutSettingsV1::disabled());
        assert!(!context_scout_settings_are_enabled(&current_settings));

        // Re-submitting the current state plans no mutation.
        let unchanged = preview_project_settings(
            &project_id,
            &current,
            ProjectSettingsPatchV1 {
                expected_revision_id: revision.as_str().to_owned(),
                context_scout: Some(false),
                ..ProjectSettingsPatchV1::default()
            },
        )
        .unwrap();
        assert!(!unchanged.changed);

        let preview = preview_project_settings(
            &project_id,
            &current,
            ProjectSettingsPatchV1 {
                expected_revision_id: revision.as_str().to_owned(),
                context_scout: Some(true),
                ..ProjectSettingsPatchV1::default()
            },
        )
        .unwrap();
        assert!(preview.changed);
        let DirectConfigurationMutation::Batch { mutations } = preview.mutation else {
            panic!("project settings must be atomic")
        };
        let [DirectConfigurationMutation::Set { key, value, .. }] = mutations.as_slice() else {
            panic!("the flag must plan exactly one typed Set")
        };
        assert_eq!(key.as_str(), CONTEXT_SCOUT_SETTINGS_SETTING_KEY);
        let ConfigurationValueV1::ContextScoutSettings(settings) = value.as_ref() else {
            panic!("the flag must write the typed Context Scout value")
        };
        assert_eq!(settings.state, ContextScoutConfigurationStateV1::Active);
        // Only the state toggles; mode, limits, and model fields are kept.
        assert_eq!(
            (
                settings.mode,
                settings.limits,
                settings.model_path,
                settings.model_id.as_deref(),
                settings.model_timeout_secs,
            ),
            (
                ContextScoutSettingsV1::disabled().mode,
                ContextScoutSettingsV1::disabled().limits,
                None,
                None,
                None,
            )
        );
        settings.validate().expect("planned value stays canonical");
    }
}

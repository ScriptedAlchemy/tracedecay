//! Canonical typed preparation for dashboard-visible settings mutations.
//!
//! HTTP parses the transport body and renders errors. Validation, target
//! selection, typed mutation construction, and restart/resync semantics live
//! here before the shared configuration application handler performs
//! authorization, CAS, receipt issuance, status, and rollback.

use serde::Deserialize;
use tracedecay_domain::configuration::{
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationSnapshotV1, ConfigurationValueV1, ContextScoutConfigurationStateV1,
    ContextScoutIntentSettingsV1, ContextScoutSettingsV1, INDEX_EXCLUDE_SETTING_KEY,
    INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY,
    INDEX_MAX_FILE_SIZE_SETTING_KEY, INDEX_TRACK_CALL_SITES_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY, SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY, SettingKey,
    TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_domain::{ProjectId, validate_index_path_patterns};

use tracedecay_configuration::config::PinnedRuntimeConfiguration;
use tracedecay_contracts::{ProjectSettingsPatchInputV1, validate_project_settings_patch};
use tracedecay_global_db::configuration::contracts::types::DirectConfigurationMutation;

pub use tracedecay_contracts::SettingsValidationIssueV1;

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
    pub telemetry: Option<TelemetrySettingsPatchV1>,
    #[serde(default)]
    pub sync: Option<SyncSettingsPatchV1>,
    #[serde(default)]
    pub context_scout: Option<bool>,
    /// Replaces the Context Scout intent-pass block. Enabling it with no
    /// questions starts from [`ContextScoutIntentSettingsV1::reference_questions`];
    /// the engine URL and model are never defaulted.
    #[serde(default)]
    pub context_scout_intent: Option<ContextScoutIntentSettingsV1>,
}

/// The intent block a patch writes: an enabled block with no questions takes
/// the reference question set, then the whole block is validated.
pub fn normalized_context_scout_intent(
    mut intent: ContextScoutIntentSettingsV1,
) -> Result<ContextScoutIntentSettingsV1, SettingsValidationIssueV1> {
    if intent.enabled && intent.questions.is_empty() {
        intent.questions = ContextScoutIntentSettingsV1::reference_questions();
    }
    intent.validate().map_err(|error| {
        issue(
            "context_scout_intent",
            &format!("context_scout_intent is invalid: {error}"),
        )
    })?;
    Ok(intent)
}

/// The effective Context Scout value in one configuration snapshot.
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

#[tracing::instrument(name = "usecases.settings.preview", level = "trace", skip_all)]
pub fn preview_project_settings(
    project_id: &ProjectId,
    current: &PinnedRuntimeConfiguration,
    patch: ProjectSettingsPatchV1,
) -> Result<ProjectSettingsPreviewV1, ProjectSettingsPreviewErrorV1> {
    if &current.target().project_id != project_id {
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
    let context_scout_intent = match patch.context_scout_intent.clone() {
        Some(intent) => match normalized_context_scout_intent(intent) {
            Ok(intent) => Some(intent),
            Err(problem) => {
                issues.push(problem);
                None
            }
        },
        None => None,
    };
    if !issues.is_empty() {
        return Err(ProjectSettingsPreviewErrorV1::Validation(issues));
    }

    let layer = ConfigurationLayerIdV1::Project {
        project_id: project_id.clone(),
    };
    let current_context_scout = effective_context_scout_settings(current.snapshot());
    let expected_is_current = &expected_revision == current.revision_id();
    let supplied_values_are_current = patch
        .include
        .as_ref()
        .is_none_or(|value| value == current.config().index_paths.include_patterns())
        && patch
            .exclude
            .as_ref()
            .is_none_or(|value| value == current.config().index_paths.exclude_patterns())
        && patch
            .max_file_size
            .is_none_or(|value| value == current.config().max_file_size)
        && patch
            .extract_docstrings
            .is_none_or(|value| value == current.config().extract_docstrings)
        && patch
            .track_call_sites
            .is_none_or(|value| value == current.config().track_call_sites)
        && patch.telemetry.as_ref().is_none_or(|telemetry| {
            telemetry
                .timings
                .is_none_or(|value| value == current.config().telemetry.timings)
        })
        && patch.sync.as_ref().is_none_or(|sync| {
            sync.auto_track_pr_branches
                .is_none_or(|value| value == current.config().sync.auto_track_pr_branches)
                && sync
                    .auto_track_pr_poll_secs
                    .is_none_or(|value| value == current.config().sync.auto_track_pr_poll_secs)
        })
        && patch.context_scout.is_none_or(|value| {
            value == context_scout_settings_are_enabled(&current_context_scout)
        })
        && context_scout_intent
            .as_ref()
            .is_none_or(|intent| current_context_scout.intent.as_ref() == Some(intent));
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
    // An intent block replaces only the intent pass. Both edit one setting,
    // so they plan one typed Set.
    let context_scout_settings = (patch.context_scout.is_some() || context_scout_intent.is_some())
        .then(|| {
            let mut settings = current_context_scout.clone();
            if let Some(enabled) = patch.context_scout {
                settings.state = if enabled {
                    ContextScoutConfigurationStateV1::Active
                } else {
                    ContextScoutConfigurationStateV1::Disabled
                };
            }
            if let Some(intent) = context_scout_intent {
                settings.intent = Some(intent);
            }
            ConfigurationValueV1::ContextScoutSettings(settings)
        });
    push(
        &mut mutations,
        &layer,
        CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
        context_scout_settings,
    )?;
    if mutations.is_empty() && !expected_is_current {
        return Err(ProjectSettingsPreviewErrorV1::RevisionConflict {
            expected: expected_revision.as_str().to_owned(),
            actual: current.revision_id().as_str().to_owned(),
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
        } else if let Err(error) = validate_index_path_patterns(std::slice::from_ref(pattern)) {
            issues.push(issue(field, &error.to_string()));
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
    use std::path::PathBuf;

    use super::*;
    use tracedecay_configuration::config::registry::ConfigurationRegistry;
    use tracedecay_configuration::config::resolver::resolve_configuration;

    /// A pin over the registry defaults: the only way to hold a
    /// `PinnedRuntimeConfiguration` is through its validating constructor.
    fn pinned_defaults(
        project_id: &ProjectId,
        revision: &ConfigurationRevisionId,
    ) -> PinnedRuntimeConfiguration {
        let snapshot = resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[])
            .unwrap()
            .snapshot;
        PinnedRuntimeConfiguration::new(
            tracedecay_configuration::config::RuntimeConfigurationTarget {
                profile_root: PathBuf::from("/profile"),
                project_id: project_id.clone(),
                project_root: PathBuf::from("/project"),
            },
            revision.clone(),
            snapshot,
        )
        .unwrap()
    }

    #[test]
    fn context_scout_flag_toggles_only_the_state_of_the_effective_value() {
        let project_id = ProjectId::new("project.settings.scout").unwrap();
        let revision = ConfigurationRevisionId::new("configuration.revision.scout").unwrap();
        let current = pinned_defaults(&project_id, &revision);
        // The registry default renders the canonical stock state: off.
        let current_settings = effective_context_scout_settings(current.snapshot());
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

    fn intent_block() -> ContextScoutIntentSettingsV1 {
        ContextScoutIntentSettingsV1 {
            enabled: true,
            engine: tracedecay_domain::configuration::DecisionEngineSettingsV1 {
                base_url: "http://127.0.0.1:8088".to_owned(),
                model: Some("intent-small".to_owned()),
                timeout_ms: 250,
                max_concurrent: 2,
                max_state_bytes: 64 * 1024,
            },
            window: tracedecay_domain::configuration::IntentWindowSettingsV1 {
                max_user_turns: 3,
                max_messages: 12,
                max_bytes: 16 * 1024,
                max_age_secs: 1_800,
                freshen_budget_ms: 150,
            },
            questions: Vec::new(),
            action_fact_score_permille: 700,
            ttl_secs: 600,
            min_delivery_gap_secs: 30,
            prompt_lookup_deadline_ms: 200,
        }
    }

    #[test]
    fn context_scout_intent_patch_fills_reference_questions_and_validates() {
        let project_id = ProjectId::new("project.settings.scout").unwrap();
        let revision = ConfigurationRevisionId::new("configuration.revision.scout").unwrap();
        let current = pinned_defaults(&project_id, &revision);

        let preview = preview_project_settings(
            &project_id,
            &current,
            ProjectSettingsPatchV1 {
                expected_revision_id: revision.as_str().to_owned(),
                context_scout: Some(true),
                context_scout_intent: Some(intent_block()),
                ..ProjectSettingsPatchV1::default()
            },
        )
        .unwrap();
        assert!(preview.changed);
        let DirectConfigurationMutation::Batch { mutations } = preview.mutation else {
            panic!("project settings must be atomic")
        };
        let [DirectConfigurationMutation::Set { key, value, .. }] = mutations.as_slice() else {
            panic!("the flag and the intent block must plan exactly one typed Set")
        };
        assert_eq!(key.as_str(), CONTEXT_SCOUT_SETTINGS_SETTING_KEY);
        let ConfigurationValueV1::ContextScoutSettings(settings) = value.as_ref() else {
            panic!("the patch must write the typed Context Scout value")
        };
        assert_eq!(settings.state, ContextScoutConfigurationStateV1::Active);
        let intent = settings.intent.as_ref().expect("intent block written");
        assert_eq!(
            intent.questions,
            ContextScoutIntentSettingsV1::reference_questions()
        );
        assert_eq!(intent.engine.base_url, "http://127.0.0.1:8088");
        settings.validate().expect("planned value stays canonical");

        // A disabled block keeps an empty question set as given.
        let disabled = normalized_context_scout_intent(ContextScoutIntentSettingsV1 {
            enabled: false,
            ..intent_block()
        })
        .unwrap();
        assert!(disabled.questions.is_empty());

        let mut invalid = intent_block();
        invalid.engine.timeout_ms = 5_001;
        let Err(ProjectSettingsPreviewErrorV1::Validation(issues)) = preview_project_settings(
            &project_id,
            &current,
            ProjectSettingsPatchV1 {
                expected_revision_id: revision.as_str().to_owned(),
                context_scout_intent: Some(invalid),
                ..ProjectSettingsPatchV1::default()
            },
        ) else {
            panic!("an invalid intent block must be a validation error")
        };
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].field, "context_scout_intent");
    }
}

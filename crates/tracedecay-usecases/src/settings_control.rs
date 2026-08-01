//! Canonical typed preparation for dashboard-visible settings mutations.
//!
//! HTTP parses the transport body and renders errors. Validation, target
//! selection, typed mutation construction, and restart/resync semantics live
//! here before the shared configuration application handler performs
//! authorization, CAS, receipt issuance, status, and rollback.

use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
    INDEX_EXCLUDE_SETTING_KEY, INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_GIT_IGNORE_SETTING_KEY,
    INDEX_INCLUDE_SETTING_KEY, INDEX_MAX_FILE_SIZE_SETTING_KEY, INDEX_TRACK_CALL_SITES_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY, SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY, SettingKey,
    TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_domain::{ManifestDigest, ProjectId, UtcMicros, canonical_sha256};

use tracedecay_application::{
    ProjectSettingsPatchInputV1, UserSettingsPatchInputV1, UserSettingsPreviewErrorV1,
    prepare_user_settings_preview, validate_project_settings_patch, validate_user_settings_values,
};

use crate::config::PinnedRuntimeConfiguration;
use crate::configuration::DirectConfigurationMutation;
use crate::user_config::{ConfigSaveError, UserConfig};

pub use tracedecay_application::{
    SettingsValidationIssueV1, UserSettingsPreviewV1, UserSettingsValuesV1,
};

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

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct UserSettingsPatchV1 {
    pub expected_revision_id: String,
    #[serde(default)]
    pub upload_enabled: Option<bool>,
    #[serde(default)]
    pub watcher_debounce: Option<String>,
    #[serde(default)]
    pub extraction_timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserSettingsMutationReceiptV1 {
    pub operation_id: ManifestDigest,
    pub base_revision: String,
    pub result_revision: String,
    pub completed_at: UtcMicros,
    pub restart_recommended: bool,
    pub rollback: UserSettingsValuesV1,
}

#[derive(Clone, Debug, Serialize)]
pub struct UserSettingsOperationStatusV1 {
    pub receipt: UserSettingsMutationReceiptV1,
    pub current_revision: String,
    pub is_current: bool,
}

#[derive(Clone, Debug)]
pub enum UserSettingsOperationErrorV1 {
    Validation(Vec<SettingsValidationIssueV1>),
    RevisionConflict { expected: String, actual: String },
    Unavailable(String),
}

pub fn preview_user_settings(
    current: &UserConfig,
    patch: UserSettingsPatchV1,
) -> Result<UserSettingsPreviewV1, UserSettingsOperationErrorV1> {
    let actual = current
        .revision_id()
        .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?;
    prepare_user_settings_preview(
        &actual,
        UserSettingsValuesV1::from(current),
        UserSettingsPatchInputV1 {
            expected_revision_id: patch.expected_revision_id,
            upload_enabled: patch.upload_enabled,
            watcher_debounce: patch.watcher_debounce,
            extraction_timeout_secs: patch.extraction_timeout_secs,
        },
    )
    .map_err(user_preview_error)
}

pub fn apply_user_settings(
    mut preview: UserSettingsPreviewV1,
) -> Result<UserSettingsMutationReceiptV1, UserSettingsOperationErrorV1> {
    let current = UserConfig::load();
    let current_revision = current
        .revision_id()
        .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?;
    if current_revision != preview.expected_revision {
        return Err(UserSettingsOperationErrorV1::RevisionConflict {
            expected: preview.expected_revision,
            actual: current_revision,
        });
    }
    let current_values = UserSettingsValuesV1::from(&current);
    if current_values != preview.previous {
        return Err(UserSettingsOperationErrorV1::Unavailable(
            "user settings preview is invalid".to_owned(),
        ));
    }
    validate_user_settings_values(&preview.candidate).map_err(user_preview_error)?;
    preview.changed = preview.candidate != preview.previous;
    preview.restart_recommended = preview.candidate.watcher_debounce
        != preview.previous.watcher_debounce
        || preview.candidate.extraction_timeout_secs != preview.previous.extraction_timeout_secs;
    let result_revision = if preview.changed {
        let candidate = preview.candidate.clone();
        let mutation = UserConfig::mutate_with_recovery_if_revision(
            &preview.expected_revision,
            move |config| {
                config.upload_enabled = candidate.upload_enabled;
                config.watcher_debounce = candidate.watcher_debounce;
                config.extraction_timeout_secs = candidate.extraction_timeout_secs;
            },
        )
        .map_err(user_save_error)?;
        if let Some(backup) = mutation.backup {
            tracing::warn!(
                backup = %backup.display(),
                "corrupt user config backed up before application mutation"
            );
        }
        UserConfig::load()
            .revision_id()
            .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?
    } else {
        preview.expected_revision.clone()
    };
    let completed_at = current_micros();
    let mut receipt = UserSettingsMutationReceiptV1 {
        operation_id: canonical_sha256(&"tracedecay.user-settings.operation.pending")
            .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?,
        base_revision: preview.expected_revision,
        result_revision,
        completed_at,
        restart_recommended: preview.restart_recommended,
        rollback: preview.previous,
    };
    receipt.operation_id = user_receipt_identity(&receipt)?;
    persist_user_receipt(&receipt)?;
    Ok(receipt)
}

pub fn user_settings_status(
    operation_id: &ManifestDigest,
) -> Result<UserSettingsOperationStatusV1, UserSettingsOperationErrorV1> {
    let path = user_receipt_path(operation_id)?;
    let bytes = std::fs::read(&path)
        .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?;
    let receipt: UserSettingsMutationReceiptV1 = serde_json::from_slice(&bytes)
        .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?;
    if &receipt.operation_id != operation_id {
        return Err(UserSettingsOperationErrorV1::Unavailable(
            "user settings operation identity is invalid".to_owned(),
        ));
    }
    if user_receipt_identity(&receipt)? != receipt.operation_id {
        return Err(UserSettingsOperationErrorV1::Unavailable(
            "user settings operation receipt is invalid".to_owned(),
        ));
    }
    let current_revision = UserConfig::load()
        .revision_id()
        .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?;
    Ok(UserSettingsOperationStatusV1 {
        is_current: current_revision == receipt.result_revision,
        receipt,
        current_revision,
    })
}

pub fn rollback_user_settings(
    receipt: &UserSettingsMutationReceiptV1,
    expected_revision: &str,
) -> Result<UserSettingsMutationReceiptV1, UserSettingsOperationErrorV1> {
    if user_receipt_identity(receipt)? != receipt.operation_id {
        return Err(UserSettingsOperationErrorV1::Unavailable(
            "user settings operation receipt is invalid".to_owned(),
        ));
    }
    let current = UserConfig::load();
    let actual = current
        .revision_id()
        .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?;
    if actual != expected_revision || actual != receipt.result_revision {
        return Err(UserSettingsOperationErrorV1::RevisionConflict {
            expected: expected_revision.to_owned(),
            actual,
        });
    }
    let previous = UserSettingsValuesV1::from(&current);
    apply_user_settings(UserSettingsPreviewV1 {
        expected_revision: expected_revision.to_owned(),
        restart_recommended: receipt.rollback.watcher_debounce != previous.watcher_debounce
            || receipt.rollback.extraction_timeout_secs != previous.extraction_timeout_secs,
        changed: receipt.rollback != previous,
        previous,
        candidate: receipt.rollback.clone(),
    })
}

fn persist_user_receipt(
    receipt: &UserSettingsMutationReceiptV1,
) -> Result<(), UserSettingsOperationErrorV1> {
    let path = user_receipt_path(&receipt.operation_id)?;
    let parent = path.parent().ok_or_else(|| {
        UserSettingsOperationErrorV1::Unavailable(
            "user settings operation path has no parent".to_owned(),
        )
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?;
    let bytes = serde_json::to_vec_pretty(receipt)
        .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))?;
    let pending = path.with_extension("json.pending");
    tracedecay_runtime_core::db::DatabaseAuthority::publish_record_atomically(
        &pending,
        &path,
        &bytes,
        "user settings operation receipt",
    )
    .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))
}

fn user_receipt_identity(
    receipt: &UserSettingsMutationReceiptV1,
) -> Result<ManifestDigest, UserSettingsOperationErrorV1> {
    canonical_sha256(&(
        "tracedecay.user-settings.operation.v1",
        &receipt.base_revision,
        &receipt.result_revision,
        receipt.completed_at,
        receipt.restart_recommended,
        &receipt.rollback,
    ))
    .map_err(|error| UserSettingsOperationErrorV1::Unavailable(error.to_string()))
}

fn user_preview_error(error: UserSettingsPreviewErrorV1) -> UserSettingsOperationErrorV1 {
    match error {
        UserSettingsPreviewErrorV1::Validation(issues) => {
            UserSettingsOperationErrorV1::Validation(issues)
        }
        UserSettingsPreviewErrorV1::RevisionConflict { expected, actual } => {
            UserSettingsOperationErrorV1::RevisionConflict { expected, actual }
        }
    }
}

fn user_receipt_path(
    operation_id: &ManifestDigest,
) -> Result<std::path::PathBuf, UserSettingsOperationErrorV1> {
    let config_path = crate::user_config::config_path().ok_or_else(|| {
        UserSettingsOperationErrorV1::Unavailable(
            "user configuration path is unavailable".to_owned(),
        )
    })?;
    let root = config_path.parent().ok_or_else(|| {
        UserSettingsOperationErrorV1::Unavailable(
            "user configuration path has no parent".to_owned(),
        )
    })?;
    let digest = operation_id
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| {
            UserSettingsOperationErrorV1::Unavailable(
                "user settings operation identity is invalid".to_owned(),
            )
        })?;
    Ok(root
        .join("settings-operations")
        .join(format!("{digest}.json")))
}

impl From<&UserConfig> for UserSettingsValuesV1 {
    fn from(config: &UserConfig) -> Self {
        Self {
            upload_enabled: config.upload_enabled,
            watcher_debounce: config.watcher_debounce.clone(),
            extraction_timeout_secs: config.extraction_timeout_secs,
        }
    }
}

fn user_save_error(error: ConfigSaveError) -> UserSettingsOperationErrorV1 {
    match error {
        ConfigSaveError::RevisionConflict { expected, actual } => {
            UserSettingsOperationErrorV1::RevisionConflict { expected, actual }
        }
        error => UserSettingsOperationErrorV1::Unavailable(error.to_string()),
    }
}

fn current_micros() -> UtcMicros {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        });
    UtcMicros(micros)
}

pub fn preview_project_settings(
    project_id: &ProjectId,
    current: &PinnedRuntimeConfiguration,
    patch: ProjectSettingsPatchV1,
) -> Result<ProjectSettingsPreviewV1, ProjectSettingsPreviewErrorV1> {
    if &current.target.project_id != project_id {
        return Err(ProjectSettingsPreviewErrorV1::InvalidAuthority);
    }
    if patch.expected_revision_id != current.revision_id.as_str() {
        return Err(ProjectSettingsPreviewErrorV1::RevisionConflict {
            expected: patch.expected_revision_id,
            actual: current.revision_id.as_str().to_owned(),
        });
    }
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
    let mut mutations = Vec::new();
    push(
        &mut mutations,
        &layer,
        INDEX_INCLUDE_SETTING_KEY,
        patch
            .include
            .filter(|value| value != &current.config.include)
            .map(ConfigurationValueV1::StringList),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_EXCLUDE_SETTING_KEY,
        patch
            .exclude
            .filter(|value| value != &current.config.exclude)
            .map(ConfigurationValueV1::StringList),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_MAX_FILE_SIZE_SETTING_KEY,
        patch
            .max_file_size
            .filter(|value| *value != current.config.max_file_size)
            .map(ConfigurationValueV1::Unsigned),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY,
        patch
            .extract_docstrings
            .filter(|value| *value != current.config.extract_docstrings)
            .map(ConfigurationValueV1::Boolean),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_TRACK_CALL_SITES_SETTING_KEY,
        patch
            .track_call_sites
            .filter(|value| *value != current.config.track_call_sites)
            .map(ConfigurationValueV1::Boolean),
    )?;
    push(
        &mut mutations,
        &layer,
        INDEX_GIT_IGNORE_SETTING_KEY,
        patch
            .git_ignore
            .filter(|value| *value != current.config.git_ignore)
            .map(ConfigurationValueV1::Boolean),
    )?;
    if let Some(telemetry) = patch.telemetry {
        push(
            &mut mutations,
            &layer,
            TELEMETRY_TIMINGS_SETTING_KEY,
            telemetry
                .timings
                .filter(|value| *value != current.config.telemetry.timings)
                .map(ConfigurationValueV1::Boolean),
        )?;
    }
    if let Some(sync) = patch.sync {
        push(
            &mut mutations,
            &layer,
            SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
            sync.auto_track_pr_branches
                .filter(|value| *value != current.config.sync.auto_track_pr_branches)
                .map(ConfigurationValueV1::Boolean),
        )?;
        push(
            &mut mutations,
            &layer,
            SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
            sync.auto_track_pr_poll_secs
                .filter(|value| *value != current.config.sync.auto_track_pr_poll_secs)
                .map(ConfigurationValueV1::Unsigned),
        )?;
    }
    let changed = !mutations.is_empty();
    Ok(ProjectSettingsPreviewV1 {
        expected_revision: current.revision_id.clone(),
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
        value,
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
    fn user_preview_validates_before_any_config_write() {
        let current = UserConfig::default();
        let revision = current.revision_id().unwrap();
        let error = preview_user_settings(
            &current,
            UserSettingsPatchV1 {
                expected_revision_id: revision,
                watcher_debounce: Some("not-a-duration".to_owned()),
                ..UserSettingsPatchV1::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, UserSettingsOperationErrorV1::Validation(_)));
    }

    #[test]
    fn user_apply_candidate_validation_rejects_transport_tampering() {
        let values = UserSettingsValuesV1 {
            upload_enabled: false,
            watcher_debounce: "not-a-duration".to_owned(),
            extraction_timeout_secs: 0,
        };
        assert!(matches!(
            validate_user_settings_values(&values).map_err(user_preview_error),
            Err(UserSettingsOperationErrorV1::Validation(issues)) if issues.len() == 2
        ));
    }
}

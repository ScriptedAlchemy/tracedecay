//! Transport-neutral settings patch validation and candidate preparation.
//!
//! Concrete `UserConfig` / runtime activation and store-backed CAS stay in root
//! adapters. This module owns only the pure request-preparation rules shared by
//! CLI, MCP, and HTTP before those adapters invoke the configuration plane.

use serde::{Deserialize, Serialize};

use crate::error::ApplicationContractError;

/// One field-level settings validation failure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsValidationIssueV1 {
    pub field: String,
    pub message: String,
}

/// User-visible settings values used for preview/CAS candidate comparison.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserSettingsValuesV1 {
    pub upload_enabled: bool,
    pub watcher_debounce: String,
    pub extraction_timeout_secs: u64,
}

/// Transport-neutral user settings patch after JSON decode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserSettingsPatchInputV1 {
    pub expected_revision_id: String,
    pub upload_enabled: Option<bool>,
    pub watcher_debounce: Option<String>,
    pub extraction_timeout_secs: Option<u64>,
}

/// Preview produced before any user-config mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserSettingsPreviewV1 {
    pub expected_revision: String,
    pub previous: UserSettingsValuesV1,
    pub candidate: UserSettingsValuesV1,
    pub restart_recommended: bool,
    pub changed: bool,
}

/// Failures while preparing a user-settings mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserSettingsPreviewErrorV1 {
    Validation(Vec<SettingsValidationIssueV1>),
    RevisionConflict { expected: String, actual: String },
}

/// Project settings fields that can be validated without opening a store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSettingsPatchInputV1 {
    pub include: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
    pub max_file_size: Option<u64>,
    pub auto_track_pr_poll_secs: Option<u64>,
}

/// Minimum accepted auto-track poll interval, mirrored from root runtime policy.
pub const MIN_AUTO_TRACK_PR_POLL_SECS_V1: u64 = 60;

fn issue(field: &str, message: &str) -> SettingsValidationIssueV1 {
    SettingsValidationIssueV1 {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

/// Parse a human-readable duration label like `"15s"` or `"1m"`.
pub fn parse_duration_label(value: &str) -> Result<u64, ApplicationContractError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApplicationContractError::InvalidIdentifier {
            field: "duration label",
        });
    }
    let seconds = if let Some(secs) = value.strip_suffix('s') {
        secs.trim().parse::<u64>().ok()
    } else if let Some(mins) = value.strip_suffix('m') {
        mins.trim()
            .parse::<u64>()
            .ok()
            .and_then(|minutes| minutes.checked_mul(60))
    } else {
        value.parse::<u64>().ok()
    };
    seconds
        .filter(|amount| *amount > 0)
        .ok_or(ApplicationContractError::InvalidRange {
            field: "duration label",
        })
}

pub fn validate_user_settings_values(
    values: &UserSettingsValuesV1,
) -> Result<(), UserSettingsPreviewErrorV1> {
    let mut issues = Vec::new();
    if parse_duration_label(&values.watcher_debounce).is_err() {
        issues.push(issue(
            "watcher_debounce",
            "watcher_debounce must be a duration like \"2s\", \"15s\", or \"1m\"",
        ));
    }
    if values.extraction_timeout_secs == 0 {
        issues.push(issue(
            "extraction_timeout_secs",
            "extraction_timeout_secs must be at least 1 second",
        ));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(UserSettingsPreviewErrorV1::Validation(issues))
    }
}

/// Validate a user-settings patch and build the CAS candidate without I/O.
pub fn prepare_user_settings_preview(
    actual_revision: &str,
    previous: UserSettingsValuesV1,
    patch: UserSettingsPatchInputV1,
) -> Result<UserSettingsPreviewV1, UserSettingsPreviewErrorV1> {
    if patch.expected_revision_id != actual_revision {
        return Err(UserSettingsPreviewErrorV1::RevisionConflict {
            expected: patch.expected_revision_id,
            actual: actual_revision.to_owned(),
        });
    }
    let mut issues = Vec::new();
    if let Some(debounce) = &patch.watcher_debounce
        && parse_duration_label(debounce).is_err()
    {
        issues.push(issue(
            "watcher_debounce",
            "watcher_debounce must be a duration like \"2s\", \"15s\", or \"1m\"",
        ));
    }
    if patch.extraction_timeout_secs == Some(0) {
        issues.push(issue(
            "extraction_timeout_secs",
            "extraction_timeout_secs must be at least 1 second",
        ));
    }
    if !issues.is_empty() {
        return Err(UserSettingsPreviewErrorV1::Validation(issues));
    }
    let candidate = UserSettingsValuesV1 {
        upload_enabled: patch.upload_enabled.unwrap_or(previous.upload_enabled),
        watcher_debounce: patch
            .watcher_debounce
            .unwrap_or_else(|| previous.watcher_debounce.clone()),
        extraction_timeout_secs: patch
            .extraction_timeout_secs
            .unwrap_or(previous.extraction_timeout_secs),
    };
    validate_user_settings_values(&candidate)?;
    Ok(UserSettingsPreviewV1 {
        expected_revision: actual_revision.to_owned(),
        restart_recommended: candidate.watcher_debounce != previous.watcher_debounce
            || candidate.extraction_timeout_secs != previous.extraction_timeout_secs,
        changed: candidate != previous,
        previous,
        candidate,
    })
}

/// Validate project settings fields that do not require store or glob crates.
///
/// Exact glob syntax remains an adapter concern; this rejects empty/control
/// patterns and zeroed numeric bounds so every surface shares the same fail-
/// closed gates before mutation construction.
pub fn validate_project_settings_patch(
    patch: &ProjectSettingsPatchInputV1,
) -> Result<(), Vec<SettingsValidationIssueV1>> {
    let mut issues = Vec::new();
    for (field, globs) in [("include", &patch.include), ("exclude", &patch.exclude)] {
        if let Some(globs) = globs {
            for pattern in globs {
                if pattern.trim().is_empty() || pattern.chars().any(char::is_control) {
                    issues.push(issue(field, &format!("{field} patterns must not be empty")));
                }
            }
        }
    }
    if patch.max_file_size == Some(0) {
        issues.push(issue(
            "max_file_size",
            "max_file_size must be at least 1 byte",
        ));
    }
    if let Some(seconds) = patch.auto_track_pr_poll_secs
        && seconds < MIN_AUTO_TRACK_PR_POLL_SECS_V1
    {
        issues.push(issue(
            "auto_track_pr_poll_secs",
            &format!(
                "auto_track_pr_poll_secs must be at least {MIN_AUTO_TRACK_PR_POLL_SECS_V1} seconds"
            ),
        ));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_preview_validates_before_candidate_merge() {
        let previous = UserSettingsValuesV1 {
            upload_enabled: false,
            watcher_debounce: "2s".to_owned(),
            extraction_timeout_secs: 30,
        };
        let error = prepare_user_settings_preview(
            "revision.user-1",
            previous.clone(),
            UserSettingsPatchInputV1 {
                expected_revision_id: "revision.user-1".to_owned(),
                extraction_timeout_secs: Some(0),
                ..UserSettingsPatchInputV1::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, UserSettingsPreviewErrorV1::Validation(_)));

        let preview = prepare_user_settings_preview(
            "revision.user-1",
            previous,
            UserSettingsPatchInputV1 {
                expected_revision_id: "revision.user-1".to_owned(),
                upload_enabled: Some(true),
                watcher_debounce: Some("15s".to_owned()),
                extraction_timeout_secs: Some(45),
            },
        )
        .unwrap();
        assert!(preview.changed);
        assert!(preview.restart_recommended);
        assert!(preview.candidate.upload_enabled);
    }

    #[test]
    fn project_patch_rejects_zero_bounds_and_empty_globs() {
        let issues = validate_project_settings_patch(&ProjectSettingsPatchInputV1 {
            include: Some(vec![String::new()]),
            max_file_size: Some(0),
            auto_track_pr_poll_secs: Some(1),
            ..ProjectSettingsPatchInputV1::default()
        })
        .unwrap_err();
        assert!(issues.iter().any(|issue| issue.field == "include"));
        assert!(issues.iter().any(|issue| issue.field == "max_file_size"));
        assert!(
            issues
                .iter()
                .any(|issue| issue.field == "auto_track_pr_poll_secs")
        );
    }
}

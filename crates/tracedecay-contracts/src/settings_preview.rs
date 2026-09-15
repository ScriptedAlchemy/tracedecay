//! Transport-neutral settings patch validation and candidate preparation.
//!
//! Store-backed CAS stays in the canonical configuration application
//! operation. This module owns only project patch validation shared before
//! adapters invoke that operation.

use serde::{Deserialize, Serialize};

/// One field-level settings validation failure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsValidationIssueV1 {
    pub field: String,
    pub message: String,
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

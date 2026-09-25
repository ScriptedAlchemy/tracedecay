//! Explicit executables behind on-demand LCM summarization.
//!
//! The daemon asks a host CLI for a summary only when this setting names the
//! executable. There is no fallback: an unconfigured provider is a typed state
//! the compaction journey reports as pending, never a `PATH` or environment
//! lookup that could reach whatever binary the operator's shell resolves.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::DomainError;

/// Bounds for a configured summary run's wall-clock budget: below the floor a
/// real model turn cannot finish, and above the ceiling a stuck provider would
/// outlive the compaction or automation run waiting on it.
pub const LCM_SUMMARIZER_TIMEOUT_SECS_RANGE: std::ops::RangeInclusive<u64> = 5..=300;

/// One provider's summarizer executable: absent by default, or the exact
/// absolute path the operator configured with its optional tuning.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum LcmSummarizerExecutableV1 {
    #[default]
    Unconfigured,
    Configured {
        canonical_path: PathBuf,
        /// Model requested for summary turns; the provider default when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Wall-clock budget for one summary run, within
        /// [`LCM_SUMMARIZER_TIMEOUT_SECS_RANGE`]; the caller's default when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
}

impl LcmSummarizerExecutableV1 {
    pub fn configured(canonical_path: PathBuf) -> Result<Self, DomainError> {
        Self::configured_with(canonical_path, None, None)
    }

    pub fn configured_with(
        canonical_path: PathBuf,
        model: Option<String>,
        timeout_secs: Option<u64>,
    ) -> Result<Self, DomainError> {
        let executable = Self::Configured {
            canonical_path,
            model,
            timeout_secs,
        };
        executable.validate()?;
        Ok(executable)
    }

    /// The configured path, or `None` while the provider is unconfigured.
    pub fn canonical_path(&self) -> Option<&Path> {
        match self {
            Self::Unconfigured => None,
            Self::Configured { canonical_path, .. } => Some(canonical_path),
        }
    }

    /// The configured model, or `None` for the provider default.
    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Unconfigured => None,
            Self::Configured { model, .. } => model.as_deref(),
        }
    }

    /// The configured run budget, or `None` for the caller's default.
    pub fn timeout(&self) -> Option<Duration> {
        match self {
            Self::Unconfigured => None,
            Self::Configured { timeout_secs, .. } => timeout_secs.map(Duration::from_secs),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Unconfigured => Ok(()),
            Self::Configured {
                canonical_path,
                model,
                timeout_secs,
            } => {
                if !canonical_path.is_absolute()
                    || canonical_path.components().any(|component| {
                        matches!(component, Component::CurDir | Component::ParentDir)
                    })
                {
                    return Err(DomainError::NonCanonical {
                        field: "lcm summarizer executable path",
                    });
                }
                if model
                    .as_deref()
                    .is_some_and(|model| model.trim().is_empty())
                {
                    return Err(DomainError::Empty {
                        field: "lcm summarizer model",
                    });
                }
                if timeout_secs
                    .is_some_and(|secs| !LCM_SUMMARIZER_TIMEOUT_SECS_RANGE.contains(&secs))
                {
                    return Err(DomainError::InvalidRange {
                        field: "lcm summarizer timeout_secs",
                    });
                }
                Ok(())
            }
        }
    }
}

/// The per-provider summarizer executables one configuration snapshot admits.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmSummarizerExecutablesV1 {
    /// `cursor-agent`, asked for a summary of Cursor sessions.
    #[serde(default)]
    pub cursor_agent: LcmSummarizerExecutableV1,
    /// `codex`, driven over app-server JSON-RPC for Codex sessions.
    #[serde(default)]
    pub codex: LcmSummarizerExecutableV1,
}

impl LcmSummarizerExecutablesV1 {
    /// Every provider unconfigured: the registry default.
    pub const fn unconfigured() -> Self {
        Self {
            cursor_agent: LcmSummarizerExecutableV1::Unconfigured,
            codex: LcmSummarizerExecutableV1::Unconfigured,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.cursor_agent.validate()?;
        self.codex.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_executable_requires_an_absolute_clean_path() {
        let absolute_base = std::env::temp_dir();
        let non_canonical = Err(DomainError::NonCanonical {
            field: "lcm summarizer executable path",
        });
        assert_eq!(
            LcmSummarizerExecutableV1::configured(PathBuf::from("cursor-agent")),
            non_canonical
        );
        assert_eq!(
            LcmSummarizerExecutableV1::configured(absolute_base.join("opt/../bin/cursor-agent")),
            non_canonical
        );
        let clean = absolute_base.join("bin").join("cursor-agent");
        let configured = LcmSummarizerExecutableV1::configured(clean.clone()).unwrap();
        assert_eq!(configured.canonical_path(), Some(clean.as_path()));
    }

    #[test]
    fn executables_decode_from_tagged_states_and_absent_providers_are_unconfigured() {
        assert_eq!(
            serde_json::to_value(LcmSummarizerExecutablesV1::unconfigured()).unwrap(),
            serde_json::json!({
                "cursor_agent": {"state": "unconfigured"},
                "codex": {"state": "unconfigured"},
            })
        );
        let decoded: LcmSummarizerExecutablesV1 = serde_json::from_value(serde_json::json!({
            "cursor_agent": {"state": "configured", "canonical_path": "/opt/bin/cursor-agent"},
        }))
        .unwrap();
        assert_eq!(
            decoded.cursor_agent.canonical_path(),
            Some(Path::new("/opt/bin/cursor-agent"))
        );
        assert_eq!(decoded.codex, LcmSummarizerExecutableV1::Unconfigured);
        assert_eq!(decoded.cursor_agent.model(), None);
        assert_eq!(decoded.cursor_agent.timeout(), None);
    }

    #[test]
    fn configured_tuning_decodes_and_rejects_out_of_range_values() {
        let path = std::env::temp_dir().join("bin").join("codex");
        let decoded: LcmSummarizerExecutablesV1 = serde_json::from_value(serde_json::json!({
            "codex": {
                "state": "configured",
                "canonical_path": path,
                "model": "summary-model",
                "timeout_secs": 5,
            },
        }))
        .unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.codex.model(), Some("summary-model"));
        assert_eq!(decoded.codex.timeout(), Some(Duration::from_secs(5)));

        for timeout_secs in [4, 301] {
            assert_eq!(
                LcmSummarizerExecutableV1::configured_with(path.clone(), None, Some(timeout_secs)),
                Err(DomainError::InvalidRange {
                    field: "lcm summarizer timeout_secs",
                })
            );
        }
        assert_eq!(
            LcmSummarizerExecutableV1::configured_with(path, Some("  ".to_owned()), None),
            Err(DomainError::Empty {
                field: "lcm summarizer model",
            })
        );
    }
}

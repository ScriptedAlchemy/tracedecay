//! Explicit executables behind on-demand LCM summarization.
//!
//! The daemon asks a host CLI for a summary only when this setting names the
//! executable. There is no fallback: an unconfigured provider is a typed state
//! the compaction journey reports as pending, never a `PATH` or environment
//! lookup that could reach whatever binary the operator's shell resolves.

use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::DomainError;

/// One provider's summarizer executable: absent by default, or the exact
/// absolute path the operator configured.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum LcmSummarizerExecutableV1 {
    #[default]
    Unconfigured,
    Configured {
        canonical_path: PathBuf,
    },
}

impl LcmSummarizerExecutableV1 {
    pub fn configured(canonical_path: PathBuf) -> Result<Self, DomainError> {
        let executable = Self::Configured { canonical_path };
        executable.validate()?;
        Ok(executable)
    }

    /// The configured path, or `None` while the provider is unconfigured.
    pub fn canonical_path(&self) -> Option<&Path> {
        match self {
            Self::Unconfigured => None,
            Self::Configured { canonical_path } => Some(canonical_path),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Unconfigured => Ok(()),
            Self::Configured { canonical_path } => {
                if !canonical_path.is_absolute()
                    || canonical_path.components().any(|component| {
                        matches!(component, Component::CurDir | Component::ParentDir)
                    })
                {
                    return Err(DomainError::NonCanonical {
                        field: "lcm summarizer executable path",
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
        assert!(LcmSummarizerExecutableV1::configured(PathBuf::from("cursor-agent")).is_err());
        assert!(
            LcmSummarizerExecutableV1::configured(absolute_base.join("opt/../bin/cursor-agent"))
                .is_err()
        );
        let clean = absolute_base.join("bin").join("cursor-agent");
        let configured = LcmSummarizerExecutableV1::configured(clean.clone()).unwrap();
        assert_eq!(configured.canonical_path(), Some(clean.as_path()));
    }

    #[test]
    fn default_is_unconfigured_for_every_provider() {
        let executables = LcmSummarizerExecutablesV1::default();
        assert_eq!(executables, LcmSummarizerExecutablesV1::unconfigured());
        assert!(executables.cursor_agent.canonical_path().is_none());
        assert!(executables.codex.canonical_path().is_none());
        executables.validate().unwrap();
    }

    #[test]
    fn unconfigured_round_trips_as_a_tagged_state() {
        let json = serde_json::to_value(LcmSummarizerExecutablesV1::unconfigured()).unwrap();
        assert_eq!(json["cursor_agent"]["state"], "unconfigured");
        let decoded: LcmSummarizerExecutablesV1 = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, LcmSummarizerExecutablesV1::unconfigured());
    }
}

//! Pinned semantic runtime configuration.
//!
//! Moved down from root `src/config.rs`. The configuration registry that
//! validates and defaults the `semantic.runtime.v1` setting lives beside the
//! configuration control store in this crate, and this shape is what it
//! encodes; the values themselves are owned by `tracedecay-semantic`, which is
//! below both.

use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

/// Default `FastEmbed` model id.
///
/// Defined by the semantic runtime crate so settings validation and model
/// acquisition can never disagree about the default selection.
pub use tracedecay_semantic::DEFAULT_FASTEMBED_MODEL_ID;

/// Cataloged `FastEmbed` model ids settings may select. Membership is validated
/// here without depending on the `semantic_code` acquisition module.
const CATALOGED_FASTEMBED_MODEL_IDS: &[&str] = &[DEFAULT_FASTEMBED_MODEL_ID];

const MAX_SEMANTIC_MODEL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SEMANTIC_TOKENIZER_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SEMANTIC_RESIDENT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_SEMANTIC_THREADS: u32 = 64;
const MAX_SEMANTIC_CONCURRENT_SESSIONS: u32 = 64;
const MAX_SEMANTIC_BATCH_SIZE: u32 = 4096;
const MAX_SEMANTIC_SEQUENCE_LENGTH: u32 = 32768;
const MAX_SEMANTIC_LOAD_DEADLINE_MS: u64 = 10 * 60 * 1000;

/// One explicitly installed local profile. Runtime code receives this path
/// from the pinned configuration snapshot and never searches an ambient model
/// cache or derives a download location from the profile identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProfileSelection {
    pub profile_id: String,
    pub accepted_profile_digest: tracedecay_domain::ManifestDigest,
    pub artifact_digest: String,
    pub artifact_path: PathBuf,
}

impl SemanticProfileSelection {
    fn validate(&self) -> Result<()> {
        self.accepted_profile_digest
            .validate()
            .map_err(|error| config_error(format!("semantic accepted profile digest: {error}")))?;
        if self.profile_id.trim().is_empty() || self.profile_id.len() > 128 {
            return Err(config_error(
                "semantic profile_id must be non-empty and at most 128 bytes",
            ));
        }
        if self.artifact_digest.len() != 64
            || !self
                .artifact_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(config_error(
                "semantic artifact_digest must be 64 lowercase hexadecimal characters",
            ));
        }
        if !self.artifact_path.is_absolute()
            || self
                .artifact_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(config_error(
                "semantic artifact_path must be an absolute normalized local path",
            ));
        }
        Ok(())
    }
}

/// Process ceilings applied before an installed semantic profile is admitted.
///
/// The selected artifact manifest may impose tighter limits. These local
/// ceilings never authorize a profile to exceed its own declared bounds. The
/// shape is owned by the semantic runtime crate; the supported ranges below
/// remain configuration-private.
pub use tracedecay_semantic::SemanticResourceCeilings;

fn validate_semantic_resource_ceilings(ceilings: SemanticResourceCeilings) -> Result<()> {
    let valid = ceilings.max_model_bytes > 0
        && ceilings.max_model_bytes <= MAX_SEMANTIC_MODEL_BYTES
        && ceilings.max_tokenizer_bytes > 0
        && ceilings.max_tokenizer_bytes <= MAX_SEMANTIC_TOKENIZER_BYTES
        && ceilings.max_resident_bytes > 0
        && ceilings.max_resident_bytes <= MAX_SEMANTIC_RESIDENT_BYTES
        && ceilings.max_model_bytes <= ceilings.max_resident_bytes
        && ceilings.max_tokenizer_bytes <= ceilings.max_resident_bytes
        && (1..=MAX_SEMANTIC_THREADS).contains(&ceilings.max_threads)
        && (1..=MAX_SEMANTIC_CONCURRENT_SESSIONS).contains(&ceilings.max_concurrent_sessions)
        && (1..=MAX_SEMANTIC_BATCH_SIZE).contains(&ceilings.max_batch_size)
        && (1..=MAX_SEMANTIC_SEQUENCE_LENGTH).contains(&ceilings.max_sequence_length)
        && (1..=MAX_SEMANTIC_LOAD_DEADLINE_MS).contains(&ceilings.load_deadline_ms);
    if !valid {
        return Err(config_error(
            "semantic resource ceilings are zero, incoherent, or exceed supported maxima",
        ));
    }
    Ok(())
}

/// Pinned semantic runtime selection.
///
/// A catalog model id (default `JinaEmbeddingsV2BaseCode`) selects the
/// `FastEmbed` package `TraceDecay` will acquire in the background. `None`
/// disables the optional semantic lane while exact, lexical, and graph
/// retrieval remain healthy. Installed local profiles remain explicit and are
/// never inferred by scanning an ambient model cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfig {
    /// Cataloged `FastEmbed` model id, or `None` to disable semantics.
    #[serde(default = "default_selected_fastembed_model")]
    pub selected_model: Option<String>,
    /// When true, first daemon startup / selection queues background download.
    #[serde(default = "default_true")]
    pub auto_download: bool,
    #[serde(default)]
    pub active_profile: Option<SemanticProfileSelection>,
    #[serde(default)]
    pub rollback_profile: Option<SemanticProfileSelection>,
    #[serde(default)]
    pub resources: SemanticResourceCeilings,
}

fn default_selected_fastembed_model() -> Option<String> {
    Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned())
}

fn default_true() -> bool {
    true
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            selected_model: default_selected_fastembed_model(),
            auto_download: true,
            active_profile: None,
            rollback_profile: None,
            resources: SemanticResourceCeilings::default(),
        }
    }
}

impl SemanticConfig {
    pub fn validate(&self) -> Result<()> {
        validate_semantic_resource_ceilings(self.resources)?;
        if let Some(model_id) = self.selected_model.as_ref() {
            if model_id.trim().is_empty() || model_id.len() > 128 {
                return Err(config_error(
                    "semantic selected_model must be a non-empty catalog id at most 128 bytes",
                ));
            }
            if !CATALOGED_FASTEMBED_MODEL_IDS.contains(&model_id.as_str()) {
                return Err(config_error(format!(
                    "semantic selected_model '{model_id}' is not a cataloged FastEmbed model"
                )));
            }
        }
        if let Some(active) = self.active_profile.as_ref() {
            active.validate()?;
        }
        if let Some(rollback) = self.rollback_profile.as_ref() {
            rollback.validate()?;
            if self.active_profile.is_none() {
                return Err(config_error(
                    "semantic rollback profile requires an active profile",
                ));
            }
        }
        if self.active_profile == self.rollback_profile && self.active_profile.is_some() {
            return Err(config_error(
                "semantic active and rollback profiles must be distinct",
            ));
        }
        Ok(())
    }
}

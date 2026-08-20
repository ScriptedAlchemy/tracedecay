//! Automation contracts, deterministic policies, and state models.
//!
//! Runtime adapters and persistence remain outside this crate.

#![forbid(unsafe_code)]

pub mod artifact_policy;
pub mod backend;
pub mod config;
mod error;
pub mod evidence_budget;
#[doc(hidden)]
pub mod managed_skill_format;
mod managed_skill_model;
mod managed_skill_validation;
mod ports;
pub mod skill_frontmatter;
pub mod text;

pub mod managed_skills {
    pub use crate::managed_skill_model::{
        MATERIALIZED_SKILL_MANAGED_BY, MAX_MANAGED_SKILL_BODY_BYTES,
        MAX_MANAGED_SUPPORT_FILE_BYTES, MAX_MANAGED_SUPPORT_FILES, ManagedSkill, ManagedSkillDraft,
        ManagedSkillMaterializationScope, ManagedSkillMetadata, ManagedSkillProvenance,
        ManagedSkillSource, ManagedSkillState, ManagedSkillUpdate, ManagedSupportFile,
        SkillInstallTarget, current_metadata_timestamp, default_managed_skill_targets,
    };
    pub use crate::managed_skill_validation::{
        validate_managed_skill, validate_managed_skill_update, validate_managed_support_files,
        validate_skill_id,
    };
}

pub use error::{AutomationError, BoxError, Result};
pub use ports::AutomationRunRecord;

pub(crate) fn config_error(message: impl Into<String>) -> AutomationError {
    AutomationError::config(message)
}

#[cfg(test)]
mod contract_tests;

//! Runtime compatibility surface for leaf-owned managed-skill contracts.

pub use tracedecay_automation::managed_skills::{
    MATERIALIZED_SKILL_MANAGED_BY, MAX_MANAGED_SKILL_BODY_BYTES, MAX_MANAGED_SUPPORT_FILE_BYTES,
    MAX_MANAGED_SUPPORT_FILES, ManagedSkill, ManagedSkillDraft, ManagedSkillMaterializationScope,
    ManagedSkillMetadata, ManagedSkillPendingUpdate, ManagedSkillProvenance, ManagedSkillSource,
    ManagedSkillState, ManagedSkillUpdate, ManagedSupportFile, SkillInstallTarget,
    current_metadata_timestamp, default_managed_skill_targets,
};

//! Automation records the runtime persists and the inspection tools return:
//! run ledger rows, managed-skill packages with their usage summaries, and
//! the Hermes skill inventory.

mod hermes;
mod managed_skills;
mod run_ledger;

pub use hermes::{
    HermesPendingSkillWrite, HermesSkillBridgeContracts, HermesSkillBridgeSnapshot,
    HermesSkillSummary,
};
pub use managed_skills::{
    ManagedSkill, ManagedSkillMaterializationScope, ManagedSkillMetadata, ManagedSkillProvenance,
    ManagedSkillSource, ManagedSkillState, ManagedSupportFile, SkillImprovementRecommendation,
    SkillInstallTarget, SkillStaleRecommendation, SkillUsageRecord, default_managed_skill_targets,
};
pub use run_ledger::{
    AgentTaskFailureClass, AgentTaskKind, AgentTaskRetryAttempt, AutomationRunArtifact,
    AutomationRunArtifactKind, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
};

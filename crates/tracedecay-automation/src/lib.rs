//! Root-free automation parsing primitives.

pub mod config;
mod error;
pub mod managed_skill_format;
pub mod managed_skill_model;
pub mod managed_skill_validation;
pub mod retention;
pub mod skill_frontmatter;
pub mod text;

pub use config::{
    AutomationBackend, AutomationConfig, AutomationConfigPatch, AutomationHostMode,
    AutomationTaskConfig, AutomationTaskPatch, AutomationTaskSet, DEFAULT_SCHEDULER_TICK_SECS,
};
pub use error::{AutomationError, Result};
pub use retention::{DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS, RetentionConfig, RetentionTable};

#[cfg(test)]
mod tests {
    #[test]
    fn truncates_prompts_on_character_boundaries() {
        assert_eq!(super::text::truncate_chars_for_prompt("a🦀bc", 2), "a🦀");
    }
}

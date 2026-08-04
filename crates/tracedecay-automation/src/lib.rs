#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::single_match)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::format_push_string)]

//! Root-free automation parsing primitives.

pub mod apply_policy;
pub mod artifact_policy;
pub mod backend;
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

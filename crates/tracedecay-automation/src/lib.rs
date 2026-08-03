//! Root-free automation parsing primitives.

mod error;
pub mod managed_skill_format;
pub mod managed_skill_model;
pub mod managed_skill_validation;
pub mod skill_frontmatter;
pub mod text;

pub use error::{AutomationError, Result};

#[cfg(test)]
mod tests {
    #[test]
    fn truncates_prompts_on_character_boundaries() {
        assert_eq!(super::text::truncate_chars_for_prompt("a🦀bc", 2), "a🦀");
    }
}

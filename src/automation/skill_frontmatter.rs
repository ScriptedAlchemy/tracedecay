//! Root compatibility shim for the automation frontmatter parser.

pub use tracedecay_agent_hosts::automation::skill_frontmatter::SkillFrontmatterValue;

use crate::errors::{Result, TraceDecayError};

pub fn parse_skill_frontmatter(
    contents: &str,
) -> Result<std::collections::BTreeMap<String, SkillFrontmatterValue>> {
    tracedecay_agent_hosts::automation::skill_frontmatter::parse_skill_frontmatter(contents)
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })
}

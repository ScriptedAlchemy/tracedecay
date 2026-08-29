//! Deterministic task classification for Claude Code turns.
//!
//! Classifies each API turn into one of 14 categories based on tool usage
//! patterns and keyword matching. Adapted from AgentSeal/codeburn (MIT).

use std::fmt;

use crate::shell::{ShellInvocation, shell_invocations, shell_words};

/// Task category for a single API turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskCategory {
    Coding,
    Debugging,
    FeatureDev,
    Refactoring,
    Testing,
    Exploration,
    Planning,
    Delegation,
    GitOps,
    BuildDeploy,
    Brainstorming,
    Conversation,
    General,
    Redundancy,
}

impl fmt::Display for TaskCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TaskCategory {
    fn strings(self) -> (&'static str, &'static str) {
        match self {
            Self::Coding => ("coding", "Coding"),
            Self::Debugging => ("debugging", "Debugging"),
            Self::FeatureDev => ("feature_dev", "Feature Dev"),
            Self::Refactoring => ("refactoring", "Refactoring"),
            Self::Testing => ("testing", "Testing"),
            Self::Exploration => ("exploration", "Exploration"),
            Self::Planning => ("planning", "Planning"),
            Self::Delegation => ("delegation", "Delegation"),
            Self::GitOps => ("git_ops", "Git Ops"),
            Self::BuildDeploy => ("build_deploy", "Build/Deploy"),
            Self::Brainstorming => ("brainstorming", "Brainstorming"),
            Self::Conversation => ("conversation", "Conversation"),
            Self::General => ("general", "General"),
            Self::Redundancy => ("redundancy", "Redundancy"),
        }
    }

    pub fn as_str(&self) -> &'static str {
        (*self).strings().0
    }

    pub fn label(&self) -> &'static str {
        (*self).strings().1
    }
}

/// Classify a turn based on tool names and Bash command content.
///
/// `tool_names`: names of all `tool_use` blocks in the turn (e.g. "Edit", "Bash").
/// `bash_commands`: content of Bash tool inputs (the `command` field).
#[hotpath::measure(label = "hosts.task_classifier.classify")]
pub fn classify(tool_names: &[&str], bash_commands: &[&str]) -> TaskCategory {
    if tool_names.is_empty() {
        return TaskCategory::Conversation;
    }

    // Agent tool → delegation
    if tool_names.contains(&"Agent") {
        return TaskCategory::Delegation;
    }

    // Planning tools
    if tool_names.contains(&"EnterPlanMode") || tool_names.contains(&"TaskCreate") {
        return TaskCategory::Planning;
    }

    // Redundancy analysis tool → dedicated bucket so dashboards can track
    // tracedecay_redundancy adoption separately from generic MCP tool calls.
    if tool_names.contains(&"tracedecay_redundancy") {
        return TaskCategory::Redundancy;
    }

    let has_edit = tool_names.contains(&"Edit") || tool_names.contains(&"Write");
    let has_read_only = tool_names.contains(&"Read")
        || tool_names.contains(&"Grep")
        || tool_names.contains(&"Glob")
        || tool_names.contains(&"WebSearch");

    let invocations = bash_commands
        .iter()
        .flat_map(|command| shell_invocations(command))
        .collect::<Vec<_>>();
    let bash_words = bash_commands
        .iter()
        .flat_map(|command| shell_words(command))
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>();

    // Git operations
    if invocations
        .iter()
        .any(|invocation| invocation.base == "git")
    {
        return TaskCategory::GitOps;
    }

    // Testing: test runner commands
    if invocations.iter().any(is_test_invocation) {
        return TaskCategory::Testing;
    }

    // Build/deploy
    if invocations.iter().any(is_build_invocation) {
        return TaskCategory::BuildDeploy;
    }

    // Debugging: error/fix keywords in bash output context
    if bash_words.iter().any(|word| {
        matches!(
            word.as_str(),
            "fix" | "debug" | "error" | "bug" | "issue" | "stacktrace" | "panic"
        )
    }) && has_edit
    {
        return TaskCategory::Debugging;
    }

    // Refactoring keywords in bash commands
    if bash_words.iter().any(|word| {
        matches!(
            word.as_str(),
            "refactor" | "rename" | "simplify" | "extract" | "inline"
        )
    }) {
        return TaskCategory::Refactoring;
    }

    // Coding: has edit/write tools
    if has_edit {
        return TaskCategory::Coding;
    }

    // Exploration: read-only tools without edits
    if has_read_only {
        return TaskCategory::Exploration;
    }

    TaskCategory::General
}

fn is_test_invocation(invocation: &ShellInvocation) -> bool {
    match invocation.base.as_str() {
        "cargo" => invocation
            .args
            .iter()
            .any(|arg| matches!(arg.as_str(), "test" | "nextest")),
        "pytest" | "py.test" | "vitest" | "jest" | "mocha" => true,
        "npm" => {
            invocation.args.first().is_some_and(|arg| arg == "test")
                || has_run_script(&invocation.args, "test")
        }
        "pnpm" | "yarn" | "bun" => has_arg(&invocation.args, "test"),
        "go" | "dotnet" | "flutter" => invocation.args.iter().any(|arg| arg == "test"),
        _ => false,
    }
}

fn is_build_invocation(invocation: &ShellInvocation) -> bool {
    match invocation.base.as_str() {
        "cargo" => invocation
            .args
            .iter()
            .any(|arg| matches!(arg.as_str(), "build" | "check" | "clippy")),
        "npm" => has_run_script(&invocation.args, "build"),
        "pnpm" | "yarn" | "bun" => has_arg(&invocation.args, "build"),
        "docker" | "kubectl" | "pm2" | "tsc" => true,
        "next" => invocation.args.iter().any(|arg| arg == "build"),
        _ => false,
    }
}

fn has_arg(args: &[String], expected: &str) -> bool {
    args.iter().any(|arg| arg == expected)
}

fn has_run_script(args: &[String], script: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == "run" && pair[1] == script)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_tools_is_conversation() {
        assert_eq!(classify(&[], &[]), TaskCategory::Conversation);
    }

    #[test]
    fn test_edit_is_coding() {
        assert_eq!(classify(&["Edit"], &[]), TaskCategory::Coding);
    }

    #[test]
    fn test_write_is_coding() {
        assert_eq!(classify(&["Write"], &[]), TaskCategory::Coding);
    }

    #[test]
    fn test_agent_is_delegation() {
        assert_eq!(classify(&["Agent", "Edit"], &[]), TaskCategory::Delegation);
    }

    #[test]
    fn test_git_bash_is_gitops() {
        assert_eq!(classify(&["Bash"], &["git status"]), TaskCategory::GitOps);
    }

    #[test]
    fn test_cargo_test_is_testing() {
        assert_eq!(
            classify(&["Bash"], &["cargo test --lib"]),
            TaskCategory::Testing
        );
    }

    #[test]
    fn test_read_only_is_exploration() {
        assert_eq!(classify(&["Read", "Grep"], &[]), TaskCategory::Exploration);
    }

    #[test]
    fn test_plan_mode_is_planning() {
        assert_eq!(classify(&["EnterPlanMode"], &[]), TaskCategory::Planning);
    }

    #[test]
    fn test_docker_is_build_deploy() {
        assert_eq!(
            classify(&["Bash"], &["docker build -t myapp ."]),
            TaskCategory::BuildDeploy
        );
    }

    #[test]
    fn quoted_command_names_do_not_drive_task_category() {
        assert_eq!(
            classify(&["Bash"], &[r#"grep "cargo test" README.md"#]),
            TaskCategory::General
        );
        assert_eq!(
            classify(&["Bash"], &["echo git status"]),
            TaskCategory::General
        );
    }

    #[test]
    fn test_fix_with_edit_is_debugging() {
        assert_eq!(
            classify(&["Bash", "Edit"], &["fix the broken import"]),
            TaskCategory::Debugging
        );
    }

    #[test]
    fn test_category_display() {
        assert_eq!(TaskCategory::GitOps.as_str(), "git_ops");
        assert_eq!(TaskCategory::GitOps.label(), "Git Ops");
    }

    #[test]
    fn test_redundancy_tool_is_redundancy() {
        assert_eq!(
            classify(&["tracedecay_redundancy"], &[]),
            TaskCategory::Redundancy
        );
    }

    #[test]
    fn test_redundancy_category_display() {
        assert_eq!(TaskCategory::Redundancy.as_str(), "redundancy");
        assert_eq!(TaskCategory::Redundancy.label(), "Redundancy");
    }
}

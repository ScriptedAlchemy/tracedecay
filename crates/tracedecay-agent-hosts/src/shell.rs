//! Shared shell-command parsing helpers for hooks and analytics classifiers.
//!
//! The parser is owned by [`tracedecay_policy::shell`] so the Context Scout
//! intent action facts and the hook classifiers read commands identically.

pub use tracedecay_policy::shell::{ShellInvocation, shell_invocations, shell_words};

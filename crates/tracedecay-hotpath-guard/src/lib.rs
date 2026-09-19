//! Process-boundary display cap for the shipped Hotpath guard.
//!
//! Published hotpath 0.24 reads `HOTPATH_FUNCTIONS_LIMIT` (then `HOTPATH_LIMIT`)
//! only when the exit report is built. Live `functions_timing` and
//! `functions_alloc` use the builder limit snapshotted when the guard starts.
//! The shipped binary applies this before that snapshot so a limit already in
//! the process environment is what MCP returns. `0` stays unlimited. A value
//! set after the process is running cannot resize the already started worker.

/// Applies the functions display limit from the process environment, if set.
///
/// Unset or unparsable variables leave the builder unchanged, matching the
/// exit report's fallback to the builder default.
pub fn with_functions_display_limit(
    builder: hotpath::HotpathGuardBuilder,
) -> hotpath::HotpathGuardBuilder {
    match functions_display_limit() {
        Some(limit) => builder.functions_limit(limit),
        None => builder,
    }
}

fn functions_display_limit() -> Option<usize> {
    parse_usize_env("HOTPATH_FUNCTIONS_LIMIT").or_else(|| parse_usize_env("HOTPATH_LIMIT"))
}

fn parse_usize_env(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|raw| raw.parse().ok())
}

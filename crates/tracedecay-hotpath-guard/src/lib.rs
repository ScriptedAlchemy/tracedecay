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

#[cfg(test)]
mod tests {
    use super::functions_display_limit;

    /// One test owns both variables: they are process-global, and the live MCP
    /// proof (`tests/functions_limit_live.rs`) only runs under `hotpath`.
    #[test]
    fn functions_limit_precedes_the_global_limit_and_ignores_junk() {
        let set = |name: &str, value: Option<&str>| unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        };

        set("HOTPATH_FUNCTIONS_LIMIT", None);
        set("HOTPATH_LIMIT", None);
        assert_eq!(functions_display_limit(), None, "unset leaves the builder");

        set("HOTPATH_LIMIT", Some("7"));
        assert_eq!(functions_display_limit(), Some(7), "HOTPATH_LIMIT is used");

        set("HOTPATH_FUNCTIONS_LIMIT", Some("2"));
        assert_eq!(
            functions_display_limit(),
            Some(2),
            "HOTPATH_FUNCTIONS_LIMIT wins"
        );

        set("HOTPATH_FUNCTIONS_LIMIT", Some("0"));
        assert_eq!(functions_display_limit(), Some(0), "0 stays unlimited");

        set("HOTPATH_FUNCTIONS_LIMIT", Some("not-a-number"));
        assert_eq!(
            functions_display_limit(),
            Some(7),
            "junk falls back to HOTPATH_LIMIT"
        );

        set("HOTPATH_LIMIT", Some("-1"));
        assert_eq!(functions_display_limit(), None, "junk in both leaves it");

        set("HOTPATH_FUNCTIONS_LIMIT", None);
        set("HOTPATH_LIMIT", None);
    }
}

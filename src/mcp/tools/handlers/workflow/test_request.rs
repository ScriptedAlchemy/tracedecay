//! Validation for the bounded `tracedecay_run_affected_tests` request.

use serde_json::Value;

use super::super::ToolResult;
use super::{MAX_TESTS_HARD_CAP, error_result};

const DEFAULT_TEST_TIMEOUT_SECS: u64 = 300;
/// Managed test runs are foreground tool effects. A caller cannot turn one
/// into an unbounded daemon job by selecting an arbitrarily distant deadline.
pub(super) const MAX_TEST_TIMEOUT_SECS: u64 = DEFAULT_TEST_TIMEOUT_SECS;

#[derive(Debug)]
pub(super) struct RunAffectedArgs {
    pub(super) explicit_paths: Option<Vec<String>>,
    pub(super) profile: String,
    pub(super) timeout_secs: u64,
    pub(super) max_tests: usize,
}

impl RunAffectedArgs {
    pub(super) fn parse(args: &Value) -> std::result::Result<Self, ToolResult> {
        let explicit_paths = match args.get("changed_paths") {
            Some(Value::Array(paths)) => {
                let mut parsed = Vec::with_capacity(paths.len());
                for path in paths {
                    let Some(path) = path.as_str() else {
                        return Err(error_result(
                            args,
                            "invalid_request",
                            "changed_paths",
                            "`changed_paths` must contain only project-relative string paths",
                        ));
                    };
                    parsed.push(path.to_owned());
                }
                Some(parsed)
            }
            Some(_) => {
                return Err(error_result(
                    args,
                    "invalid_request",
                    "changed_paths",
                    "`changed_paths` must be an array of project-relative string paths",
                ));
            }
            None => None,
        };
        let profile = args
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or("debug")
            .to_owned();
        let timeout_secs = bounded_positive_u64(
            args,
            "timeout_secs",
            DEFAULT_TEST_TIMEOUT_SECS,
            MAX_TEST_TIMEOUT_SECS,
        )?;
        let max_tests = usize::try_from(bounded_positive_u64(
            args,
            "max_tests",
            100,
            MAX_TESTS_HARD_CAP as u64,
        )?)
        .map_err(|_| {
            error_result(
                args,
                "invalid_request",
                "max_tests",
                "`max_tests` cannot be represented on this platform",
            )
        })?;

        Ok(Self {
            explicit_paths,
            profile,
            timeout_secs,
            max_tests,
        })
    }
}

fn bounded_positive_u64(
    args: &Value,
    field: &str,
    default: u64,
    maximum: u64,
) -> std::result::Result<u64, ToolResult> {
    let Some(value) = args.get(field) else {
        return Ok(default);
    };
    let Some(value) = value.as_u64() else {
        return Err(error_result(
            args,
            "invalid_request",
            field,
            &format!("`{field}` must be an integer from 1 through {maximum}"),
        ));
    };
    if !(1..=maximum).contains(&value) {
        return Err(error_result(
            args,
            "invalid_request",
            field,
            &format!("`{field}` must be an integer from 1 through {maximum}"),
        ));
    }
    Ok(value)
}

//! Validation for the bounded `tracedecay_run_affected_tests` request.

use serde_json::{Value, json};

use crate::ToolResult;
use crate::tools::render;

const DEFAULT_TEST_TIMEOUT_SECS: u64 = 300;
/// Maximum exact test identities admitted to one managed foreground request.
pub const MAX_TESTS_HARD_CAP: usize = 500;
/// Managed test runs are foreground tool effects. A caller cannot turn one
/// into an unbounded daemon job by selecting an arbitrarily distant deadline.
pub const MAX_TEST_TIMEOUT_SECS: u64 = DEFAULT_TEST_TIMEOUT_SECS;

fn error_result(args: &Value, kind: &str, operation: &str, message: &str) -> ToolResult {
    let value = json!({
        "passed": 0,
        "failed": 0,
        "results": [],
        "error": {
            "kind": kind,
            "operation": operation,
            "message": message,
        }
    });
    let text = render::finalize(None, args, &value, || render::generic_md(&value));
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        Vec::new(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestProfile {
    Debug,
    Release,
}

impl TestProfile {
    fn parse(args: &Value) -> std::result::Result<Self, ToolResult> {
        match args.get("profile") {
            None => Ok(Self::Debug),
            Some(Value::String(profile)) if profile == "debug" => Ok(Self::Debug),
            Some(Value::String(profile)) if profile == "release" => Ok(Self::Release),
            Some(_) => Err(error_result(
                args,
                "invalid_request",
                "profile",
                "`profile` must be `debug` or `release`",
            )),
        }
    }
}

#[derive(Debug)]
pub struct RunAffectedArgs {
    pub explicit_paths: Option<Vec<String>>,
    pub profile: TestProfile,
    pub timeout_secs: u64,
    pub max_tests: usize,
}

impl RunAffectedArgs {
    #[hotpath::measure(label = "mcp.workflow.affected_tests.request_build")]
    pub fn parse(args: &Value) -> std::result::Result<Self, ToolResult> {
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
        let profile = TestProfile::parse(args)?;
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

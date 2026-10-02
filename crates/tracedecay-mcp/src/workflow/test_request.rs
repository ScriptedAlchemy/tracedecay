//! Validation for the bounded `tracedecay_run_affected_tests` request.

use tracedecay_contracts::retrieval::{
    AffectedTestErrorV1, AffectedTestsNotRunV1, RunAffectedTestsSurfaceRequestV1, TestProfileV1,
};

const DEFAULT_TEST_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MAX_TESTS: u64 = 100;
/// Maximum exact test identities admitted to one managed foreground request.
pub const MAX_TESTS_HARD_CAP: usize = 500;
/// Managed test runs are foreground tool effects. A caller cannot turn one
/// into an unbounded daemon job by selecting an arbitrarily distant deadline.
pub const MAX_TEST_TIMEOUT_SECS: u64 = DEFAULT_TEST_TIMEOUT_SECS;

/// A run refused before any test was selected.
pub(crate) fn refused_run(kind: &str, operation: &str, message: &str) -> AffectedTestsNotRunV1 {
    AffectedTestsNotRunV1 {
        passed: 0,
        failed: 0,
        results: Vec::new(),
        note: None,
        error: Some(AffectedTestErrorV1 {
            kind: kind.to_owned(),
            operation: operation.to_owned(),
            message: message.to_owned(),
        }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestProfile {
    Debug,
    Release,
}

#[derive(Debug)]
pub struct RunAffectedArgs {
    pub changed_paths: Vec<String>,
    pub profile: TestProfile,
    pub timeout_secs: u64,
    pub max_tests: usize,
}

impl RunAffectedArgs {
    /// Applies the managed-run bounds to a decoded request. A bound violation
    /// is an in-band refusal, reported before any test is selected.
    #[tracing::instrument(
        name = "mcp.workflow.affected_tests.request_build",
        level = "trace",
        skip_all
    )]
    pub fn from_request(
        request: RunAffectedTestsSurfaceRequestV1,
    ) -> std::result::Result<Self, Box<AffectedTestsNotRunV1>> {
        let timeout_secs = bounded_positive(
            request.timeout_secs,
            "timeout_secs",
            DEFAULT_TEST_TIMEOUT_SECS,
            MAX_TEST_TIMEOUT_SECS,
        )?;
        let max_tests = usize::try_from(bounded_positive(
            request.max_tests,
            "max_tests",
            DEFAULT_MAX_TESTS,
            MAX_TESTS_HARD_CAP as u64,
        )?)
        .map_err(|_| {
            Box::new(refused_run(
                "invalid_request",
                "max_tests",
                "`max_tests` cannot be represented on this platform",
            ))
        })?;
        Ok(Self {
            changed_paths: request.changed_paths,
            profile: match request.profile.unwrap_or_default() {
                TestProfileV1::Debug => TestProfile::Debug,
                TestProfileV1::Release => TestProfile::Release,
            },
            timeout_secs,
            max_tests,
        })
    }
}

fn bounded_positive(
    value: Option<u64>,
    field: &str,
    default: u64,
    maximum: u64,
) -> std::result::Result<u64, Box<AffectedTestsNotRunV1>> {
    let value = value.unwrap_or(default);
    if !(1..=maximum).contains(&value) {
        return Err(Box::new(refused_run(
            "invalid_request",
            field,
            &format!("`{field}` must be an integer from 1 through {maximum}"),
        )));
    }
    Ok(value)
}

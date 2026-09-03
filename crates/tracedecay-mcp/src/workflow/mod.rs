//! Portable managed-test helpers shared by the root workflow handler.

mod test_identity;
mod test_request;
mod test_runner;

pub use test_identity::{libtest_identity, libtest_module_prefix};
pub use test_request::{MAX_TEST_TIMEOUT_SECS, MAX_TESTS_HARD_CAP, RunAffectedArgs, TestProfile};
pub use test_runner::{
    TestRunControl, TestRunFailure, TestRunOutput, TestRunStream, cargo_test_args,
    parse_libtest_output, run_cargo_tests,
};

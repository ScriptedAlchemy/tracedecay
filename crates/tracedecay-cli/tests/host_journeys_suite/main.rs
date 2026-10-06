//! Consolidated `test-transport` host-journey acceptance suite.
//!
//! Both modules were standalone integration-test binaries gated on the same
//! `test-transport` feature, and each one linked the complete CLI dependency
//! closure. Compiled as modules of one binary, each test keeps its old binary
//! name as its module prefix. `work_loop_journey` stays its own binary because
//! it mutates process environment variables in-process.

mod host_lifecycle_cli_acceptance;
#[path = "../../../../tests/support/isolated_profile.rs"]
mod isolated_profile;

/// The `tracedecay` binary this suite drives: the runfiles path Bazel and
/// nextest pass at run time, else the artifact Cargo compiled in.
pub(crate) fn tracedecay_exe() -> &'static std::path::Path {
    static EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        std::env::var_os("CARGO_BIN_EXE_tracedecay")
            .map_or_else(|| env!("CARGO_BIN_EXE_tracedecay").into(), Into::into)
    })
}

//! Consolidated core-engine + CLI integration suite.
//!
//! Windows CI links every integration-test binary separately, and link time
//! dominates the suite. The formerly standalone binaries below are compiled
//! as modules of this single binary instead; each test keeps its old binary
//! name as the module prefix (e.g. `tracedecay_test::test_get_all_files`),
//! which the Windows test-group filters in `.config/nextest.toml` match on.
//!
//! The build-support and packaging checks (`build_version_test`,
//! `cli_boundary`, `dashboard_bundle_test`, `source_provenance_test`) were
//! the crate's remaining default-feature singles and are modules here for the
//! same reason.

mod build_version_test;
mod cli_boundary;
mod cli_non_interactive_test;
#[path = "../../../tracedecay/tests/common/mod.rs"]
mod common;
mod config_test;
mod dashboard_bundle_test;
mod gain_test;
mod monitor_test;
// Mounted for cli_non_interactive_test, which exercises the compiled
// host-CLI fixture provisioner.
#[path = "../../build-support/provision_host_cli_fixture.rs"]
mod provision_host_cli_fixture;
#[cfg(unix)]
mod remote_cli_test;
mod source_provenance_test;
mod sync_test;
mod test_profile_isolation_test;
#[cfg(unix)]
mod tool_cursor_test;
#[cfg(unix)]
mod tool_daemon_test;
mod tool_discovery_test;
mod tool_first_touch_test;
#[cfg(unix)]
mod tool_surface_transport_test;
mod tracedecay_test;
mod user_config_test;

/// The `tracedecay` binary this suite drives: the runfiles path Bazel and
/// nextest pass at run time, else the artifact Cargo compiled in.
pub(crate) fn tracedecay_exe() -> &'static std::path::Path {
    static EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        std::env::var_os("CARGO_BIN_EXE_tracedecay")
            .map_or_else(|| env!("CARGO_BIN_EXE_tracedecay").into(), Into::into)
    })
}

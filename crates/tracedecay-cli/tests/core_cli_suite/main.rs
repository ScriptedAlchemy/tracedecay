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
#[cfg(unix)]
mod observation_reset_recovery_test;
#[path = "../../build-support/provision_host_cli_fixture.rs"]
mod provision_host_cli_fixture;
mod semantic_activation_test;
mod source_provenance_test;
mod sync_test;
mod test_profile_isolation_test;
#[cfg(unix)]
mod tool_daemon_test;
mod tool_first_touch_test;
#[cfg(unix)]
mod tool_surface_transport_test;
mod tracedecay_test;
mod user_config_test;

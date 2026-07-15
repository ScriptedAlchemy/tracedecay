//! Consolidated core-engine + CLI integration suite.
//!
//! Windows CI links every integration-test binary separately, and link time
//! dominates the suite. The formerly standalone binaries below are compiled
//! as modules of this single binary instead; each test keeps its old binary
//! name as the module prefix (e.g. `tracedecay_test::test_get_all_files`),
//! which the Windows test-group filters in `.config/nextest.toml` match on.

#[path = "../common/mod.rs"]
mod common;

mod cli_help_test;
mod cli_non_interactive_test;
mod config_test;
mod gain_test;
mod integration_test;
mod monitor_test;
mod regression_core_engine_test;
mod sync_test;
mod test_profile_isolation_test;
#[cfg(unix)]
mod tool_daemon_test;
mod tool_first_touch_test;
mod tracedecay_test;
mod user_config_test;
mod walk_up_test;

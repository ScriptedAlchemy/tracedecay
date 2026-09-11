//! Consolidated runtime-core integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix.
//!
//! Deliberately still separate binaries: `git_discovery_authority` sets the
//! `GIT` environment variable in-process, `git_repository_authority` re-executes
//! its own binary with a bare `--exact` test name, `git_repository_authority_hotpath`
//! installs a counting global allocator and binds fixed metrics ports, and
//! `sqlite_foreign_snapshot_cost` reads the process-wide `/proc/self/io` counters.

mod bounded_git_command;
mod git_discovery;
mod git_topology;
mod native_integration_authority;

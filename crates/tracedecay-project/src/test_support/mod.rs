//! Test-only fixtures composed over the project handle. Compiled under
//! `cfg(test)` or `test-helpers`, never into a default library build.

pub mod host_admission;
#[allow(clippy::too_many_lines)]
pub mod session_temporal_benchmark;

//! Test-only composition-root fixtures. Compiled under `cfg(test)` or
//! `test-helpers`, never into a default or `production` library build.

pub mod git;
pub mod host_admission;

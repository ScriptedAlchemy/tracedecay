//! Test-only composition-root fixtures. Compiled under `cfg(test)` or
//! `test-helpers`, never into a default or `production` library build.

pub mod git;
#[cfg(any(test, feature = "test-transport"))]
pub mod host_admission;

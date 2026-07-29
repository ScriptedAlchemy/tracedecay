//! Final workspace dependency-direction guard.
//!
//! Compile-time package tests and direct journeys protect behavior. This target
//! retains only the Cargo dependency check that prevents extracted crates from
//! depending back on the root package.

mod compile_isolation;

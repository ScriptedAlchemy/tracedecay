//! Session-temporal benchmark contract, environment isolation, and runner
//! coverage for the harness this crate owns under `test_support`.

#![allow(clippy::too_many_lines)]

mod temporal_benchmark;
#[cfg(unix)]
mod temporal_benchmark_runner;

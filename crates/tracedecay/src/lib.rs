// Required for the hotpath feature: layout computation for the boxed
// `_inner` async bodies in daemon::bootstrap::run_foreground,
// daemon::core_doctor::write_doctor_runtime_response, and
// daemon::projectless::serve_projectless_client overflows the default query
// depth (each reports "query depth increased by 130").
#![recursion_limit = "256"]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
// Pedantic style lints allowed crate-wide (consistent with the allows above):
// these are non-correctness stylistic findings whose "fixes" are signature or
// control-flow churn that would ripple across callers.
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::unused_self)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::option_option)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::ref_option)]
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::manual_async_fn)]
#![allow(clippy::unused_async)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::if_not_else)]
#![allow(clippy::fn_params_excessive_bools)]
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::missing_fields_in_debug)]
#![allow(clippy::single_match_else)]
// Several async fns and their test drivers hold futures that cross the 16KB
// pedantic threshold only on Windows' struct layout; boxing every await site
// across the tree churns far more than the lint is worth here.
#![allow(clippy::large_futures)]

pub mod agents;
pub use tracedecay_agent_hosts::cli_fallback_args_invocation_lit;
pub mod application_surface;
// Fixture surface for integration tests, assembled by the composition root.
// Gated so a default or `production` build carries none of it.
#[cfg(any(test, feature = "test-helpers"))]
pub mod host_admission;
pub use tracedecay_code_index::ast_grep_search;
pub mod bench;
pub mod catalog_composition;
pub mod cloud;
pub use tracedecay_code_index as code_index;
pub use tracedecay_query as query;
pub mod config;
pub mod daemon;
pub mod dashboard;
pub mod doctor;
pub use tracedecay_usecases::git_query;
pub mod graph;
mod hooks;
#[cfg(test)]
mod host_admission_test;
pub mod mcp;
pub mod product_runtime;
pub use product_runtime::{
    ProductRuntimeError, ProductRuntimeProvider, ProductSourceProvenance, product_runtime,
    register_product_runtime,
};
pub mod profile_registry_maintenance;
mod project_store_runtime;
mod runtime_ports;
pub use runtime_ports::{hook_runtime, register_runtime_ports};
#[cfg(test)]
#[path = "sessions/claude_observation_benchmark.rs"]
mod claude_observation_benchmark;
pub mod runtime_telemetry;
pub mod serve;
#[cfg(test)]
#[path = "sessions/ingest_tests.rs"]
mod session_ingest_tests;
// Benchmark harness, not product surface: the shipped library must not carry
// its fixture provisioning or process-environment mutation. The `session_temporal`
// bench target and the `test-helpers` integration lanes select it explicitly.
#[cfg(any(test, feature = "test-helpers"))]
pub mod session_temporal_benchmark;
pub mod tracedecay;
#[doc(hidden)]
pub mod vector_generation_test_support;
pub mod version;
#[cfg(test)]
#[path = "sessions/workflow_ingest_tests.rs"]
mod workflow_ingest_tests;

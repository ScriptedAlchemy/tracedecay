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

// Query-bench implementation for `tracedecay bench` / MCP `admin_project`
// action `bench`. Kept off the default library graph; the CLI production
// feature selects `bench` so the shipped command still compiles.
#[cfg(any(test, feature = "bench", feature = "test-helpers"))]
#[path = "../benches/query_bench/harness.rs"]
pub mod bench;
// Fixture surface for integration tests, assembled by the composition root.
// Gated so a default or `production` build carries none of it.
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_support;
#[cfg(any(test, feature = "test-helpers"))]
pub use test_support::host_admission;
pub use tracedecay_code_index as code_index;
pub use tracedecay_query as query;
pub mod config;
pub mod daemon;
pub mod dashboard;
pub mod doctor;
pub use tracedecay_application::git_query;
mod hooks;
#[cfg(test)]
mod host_admission_test;
pub mod mcp;
pub mod product_runtime;
pub use product_runtime::{
    ProductRuntimeError, ProductRuntimeProvider, ProductSourceProvenance, product_runtime,
    register_product_runtime,
};
mod project_store_runtime;
mod runtime_ports;
pub use runtime_ports::{hook_runtime, register_runtime_ports};
pub mod serve;
// Session-temporal harness lives under `benches/`; the lib only paths it in
// when a bench target or integration lane asks for `test-helpers`.
#[cfg(any(test, feature = "test-helpers"))]
#[path = "../benches/session_temporal/harness.rs"]
pub mod session_temporal_benchmark;
pub mod tracedecay;
#[doc(hidden)]
pub mod vector_generation_test_support;
pub mod version;

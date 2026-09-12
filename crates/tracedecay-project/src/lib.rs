//! The registered `TraceDecay` project handle and the floor of the composition
//! root that it needs.
//!
//! This crate sits below the MCP and daemon layers so both can name
//! [`project::TraceDecay`] without a dependency cycle. It owns the daemon
//! configuration authority ([`config`]), the runtime-port wiring the extracted
//! crates invert ([`runtime_ports`]), the process-wide product runtime
//! ([`product_runtime`], [`version`]), and — behind `test-helpers` — the
//! registered host-admission test runtime every integration fixture composes.
//!
//! The daemon client is the one capability this crate cannot build: the
//! composition root registers it through
//! [`runtime_ports::register_runtime_ports`] before any project opens.

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
#![cfg_attr(test, allow(clippy::too_many_lines))]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::unused_self)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::option_option)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::ref_option)]
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::manual_async_fn)]
#![allow(clippy::if_not_else)]
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::missing_fields_in_debug)]
#![allow(clippy::single_match_else)]

pub mod config;
pub mod product_runtime;
pub mod project;
mod project_store_runtime;
pub mod runtime_ports;
// Fixture surface for integration tests. Gated so a default build carries
// none of it.
#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::too_many_lines)]
pub mod test_support;
pub mod version;

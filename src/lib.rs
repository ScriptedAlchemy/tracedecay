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
// control-flow churn that would ripple across callers on this in-flight
// redesign branch. Kept as allows rather than risking a co-editor's work.
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

pub mod accounting;
pub mod agents;
pub use tracedecay_agent_hosts::cli_fallback_args_invocation_lit;
mod analytics;
pub mod analytics_bridge;
pub mod application;
pub mod application_output;
pub mod application_surface;
pub use tracedecay_code_index::ast_grep_search;
pub mod automation;
pub mod bench;
pub mod branch;
pub mod branch_meta;
pub mod catalog_composition;
pub mod client_identity;
pub mod cloud;
pub use tracedecay_code_index as code_index;
pub use tracedecay_query as query;
pub mod config;
pub mod context;
pub mod daemon;
pub mod daemon_client;
pub mod daemon_contract;
pub mod dashboard;
#[cfg(test)]
#[path = "../build-support/dashboard_cache.rs"]
mod dashboard_build_cache;
#[cfg(test)]
mod dashboard_diagnostics;
pub mod db;
mod dependency_imports;
pub mod derive_table;
pub mod diagnose;
pub mod diagnostics;
pub(crate) use diagnostics::lsp::semantic::{
    graph_semantic_capabilities, production_semantic_authorities,
};
pub mod diagnostics_publication;
pub mod diagnostics_query;
pub mod diagnostics_store;
pub mod display;
pub mod doctor;
pub mod errors;
pub mod external_tools;
pub mod extraction_worker;
pub mod git;
mod git_index_transactions;
pub mod git_intelligence;
pub use tracedecay_usecases::git_query;
pub mod global_db;
pub mod graph;
pub mod hooks;
pub mod lifecycle_lease;
pub mod mcp;
pub mod memory;
pub mod migrate;
pub mod monitor;
mod open_store_holders;
mod os_str_bytes;
mod path_scope;
mod path_tree;
pub mod privacy;
pub mod project_registry;
pub mod redundancy;
mod repository_provenance;
pub mod request_identity;
pub mod resolution;
pub mod retention;
pub mod runtime_identity;
pub mod runtime_telemetry;
pub mod search_eval;
mod semantic_code;
pub mod serve;
pub mod sessions;
mod shell;
mod sqlite_read_snapshot;
pub mod storage;
pub mod store;
pub mod sync;
pub mod text;
pub mod timeutil;
pub mod tracedecay;
pub mod types;
pub mod upgrade;
pub mod user_config;
pub mod version;
pub mod worktree;

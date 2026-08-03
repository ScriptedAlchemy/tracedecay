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
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::single_match)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::format_push_string)]

pub mod accounting;
pub mod agents;
pub use tracedecay_agent_hosts::cli_fallback_args_invocation_lit;
pub(crate) use tracedecay_agent_hosts::analytics;
pub mod analytics_bridge;
pub mod ast_grep_search;
pub mod automation;
pub mod bench;
pub mod branch;
pub mod branch_meta;
pub mod client_identity;
pub mod cloud;
pub mod config;
pub mod context;
pub mod daemon;
pub mod dashboard;
pub mod db;
mod dependency_imports;
pub mod derive_table;
pub mod diagnose;
pub mod diagnostics;
pub mod display;
pub mod doctor;
pub mod errors;
pub mod external_tools;
pub mod extraction;
pub mod extraction_worker;
pub mod git;
pub mod global_db;
pub mod graph;
mod hermes_profile_config;
pub mod hooks;
pub mod lifecycle_lease;
pub mod mcp;
pub mod memory;
pub mod migrate;
pub mod monitor;
mod open_store_holders;
mod path_scope;
mod path_tree;
pub mod project_registry;
pub mod redundancy;
pub mod resolution;
pub mod retention;
pub mod runtime_identity;
pub mod runtime_telemetry;
pub mod serde_util;
pub mod serve;
pub mod sessions;
mod shell;
pub mod storage;
pub mod sync;
pub mod text;
pub mod timeutil;
pub mod tracedecay;
pub mod types;
pub mod upgrade;
pub mod user_config;
pub mod worktree;

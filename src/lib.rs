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

pub mod accounting;
pub mod agents;
mod analytics;
pub mod analytics_bridge;
pub mod application;
pub mod application_surface;
pub mod ast_grep_search;
pub mod automation;
pub mod bench;
pub mod branch;
pub mod branch_meta;
pub mod catalog_composition;
pub mod client_identity;
pub mod cloud;
pub mod code_index;
pub mod config;
pub mod context;
pub mod daemon;
pub mod daemon_client;
pub mod dashboard;
pub mod db;
mod dependency_imports;
pub mod derive_table;
pub mod diagnose;
pub mod diagnostics;
pub mod diagnostics_query;
pub mod diagnostics_store;
pub mod display;
pub mod doctor;
pub mod errors;
pub mod external_tools;
pub mod extraction;
pub mod extraction_worker;
pub mod git;
mod git_index_transactions;
pub mod git_intelligence;
pub mod git_query;
pub mod global_db;
pub mod graph;
pub mod hooks;
pub mod lifecycle_lease;
pub mod lsp_bridge;
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
pub mod query;
pub mod redundancy;
mod repository_provenance;
pub mod resolution;
pub mod retention;
pub mod runtime_identity;
pub mod runtime_telemetry;
pub mod search_eval;
// In-flight semantic retrieval/code-index feature; many APIs are staged ahead
// of their production wiring (activation landed under active development), so
// dead_code is allowed module-wide rather than deleting a co-editor's work.
#[allow(dead_code)]
mod semantic_code;
pub mod serde_util;
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
#[cfg(windows)]
mod windows_file;
pub mod worktree;
mod yaml_scalar;

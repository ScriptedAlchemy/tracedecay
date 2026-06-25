#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
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
pub mod derive_table;
pub mod diagnose;
pub mod diagnostics;
pub mod display;
pub mod doctor;
pub mod errors;
pub mod external_tools;
pub mod extraction;
pub mod extraction_worker;
pub mod global_db;
pub mod graph;
pub mod hooks;
pub mod mcp;
pub mod memory;
pub mod migrate;
pub mod monitor;
mod path_tree;
pub mod redundancy;
pub mod resolution;
pub mod runtime_telemetry;
pub mod serve;
pub mod sessions;
pub mod storage;
pub mod sync;
pub mod text;
pub mod timeutil;
pub mod tracedecay;
pub mod types;
pub mod upgrade;
pub mod user_config;
pub mod worktree;

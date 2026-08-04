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

//! Store-free LSP diagnostics support.

pub mod analyzer;

pub use analyzer::{LspError, Result, activity, adapters, broker, client, settings};

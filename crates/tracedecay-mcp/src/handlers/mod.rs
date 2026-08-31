//! Portable MCP handler adapters that depend only on application, protocol,
//! and catalog crates.

mod multi_root;
mod retained_response;

pub use multi_root::handle_multi_root;
pub use retained_response::{
    retained_problem_envelope, retained_safe_diagnostic, validated_retained_response,
};

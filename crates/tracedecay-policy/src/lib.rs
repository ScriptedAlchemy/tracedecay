//! Deterministic, side-effect-free policy evaluators for TraceDecay V2.
//!
//! This crate receives immutable snapshots and produces typed decisions. It
//! never opens storage, reads configuration, invokes a provider, starts an
//! analyzer, executes Git, renders a transport response, or performs a clock
//! lookup. Every time-dependent fact is an explicit input.

#![forbid(unsafe_code)]

pub mod analyzer;
pub mod authorization;
pub mod configuration;
pub mod diagnostic_curation;
pub mod git;
pub mod hint_delivery;
pub mod replay;
pub mod retrieval_selection;
pub mod routing;

pub use analyzer::*;
pub use authorization::*;
pub use configuration::*;
pub use diagnostic_curation::*;
pub use git::*;
pub use hint_delivery::*;
pub use replay::*;
pub use retrieval_selection::*;
pub use routing::*;

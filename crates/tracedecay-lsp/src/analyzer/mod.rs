//! Generic analyzer diagnostics runtime.

pub mod activity;
pub mod adapters;
pub mod broker;
pub mod client;
mod error;
pub mod host_ownership;
pub mod semantic;
pub mod settings;

pub use error::{AnalyzerCancellation, AnalyzerResult, AnalyzerRuntimeError};
pub use host_ownership::HostAnalyzerOwnership;
pub use semantic::{
    CompositeAnalyzerCancellation, LanguageSemanticRoute, PolyglotSemanticProvider,
};

//! Generic analyzer diagnostics runtime.

pub mod activity;
pub mod adapters;
pub mod broker;
pub mod client;
mod error;
pub mod semantic;
pub mod settings;

pub use error::{AnalyzerCancellation, AnalyzerResult, AnalyzerRuntimeError};
pub use semantic::{
    CompositeAnalyzerCancellation, LanguageSemanticRoute, PolyglotSemanticProvider,
};
